use std::{fmt, net::IpAddr};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use super::{
    plugin_protocol::{
        MIGRATION_AUTHENTICATE_USER_METHOD, MIGRATION_LIST_ITEMS_METHOD,
        MIGRATION_LIST_USERS_METHOD, MIGRATION_PERSON_FAVORITES_METHOD, MIGRATION_TEST_METHOD,
        MIGRATION_USER_STATE_METHOD,
    },
    plugins::{EMBY_MIGRATION_PLUGIN_ID, PluginService, PluginServiceError},
};

const MAX_SOURCE_URL_LENGTH: usize = 2048;
const MAX_SECRET_LENGTH: usize = 1024;
const MAX_TEXT_LENGTH: usize = 1024;
const MAX_ID_LENGTH: usize = 256;
const MAX_PAGE_SIZE: i64 = 500;
const SOURCE_USER_PREVIEW_FIELDS: [&str; 4] = ["id", "name", "isDisabled", "isAdministrator"];

#[derive(Debug, Eq, PartialEq)]
pub enum MigrationInputError {
    InvalidSourceUrl,
    PrivateNetworkNotAllowed,
    InvalidSecret,
    InvalidIdentifier,
    NoSelectedUsers,
    NoSelectedMigrationScope,
    NoSelectedItemStateFilters,
    NoSelectedTargetLibraries,
}

impl fmt::Display for MigrationInputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSourceUrl => formatter.write_str("invalid Emby source URL"),
            Self::PrivateNetworkNotAllowed => {
                formatter.write_str("private Emby network requires explicit approval")
            }
            Self::InvalidSecret => formatter.write_str("invalid Emby API key"),
            Self::InvalidIdentifier => formatter.write_str("invalid migration identifier"),
            Self::NoSelectedUsers => formatter.write_str("at least one Emby user must be selected"),
            Self::NoSelectedMigrationScope => {
                formatter.write_str("at least one migration category must be selected")
            }
            Self::NoSelectedItemStateFilters => {
                formatter.write_str("at least one media state field must be selected")
            }
            Self::NoSelectedTargetLibraries => {
                formatter.write_str("at least one target Lux library must be selected")
            }
        }
    }
}

impl std::error::Error for MigrationInputError {}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EmbyMigrationSource {
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub allow_private_network: bool,
}

impl EmbyMigrationSource {
    pub fn validate(&self) -> Result<Url, MigrationInputError> {
        let value = self.base_url.trim();
        if value.is_empty() || value.len() > MAX_SOURCE_URL_LENGTH {
            return Err(MigrationInputError::InvalidSourceUrl);
        }
        if self.api_key.trim().is_empty() || self.api_key.len() > MAX_SECRET_LENGTH {
            return Err(MigrationInputError::InvalidSecret);
        }
        let mut url = Url::parse(value).map_err(|_| MigrationInputError::InvalidSourceUrl)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(MigrationInputError::InvalidSourceUrl);
        }
        let host = url
            .host_str()
            .ok_or(MigrationInputError::InvalidSourceUrl)?
            .to_ascii_lowercase();
        if is_private_name(&host) && !self.allow_private_network {
            return Err(MigrationInputError::PrivateNetworkNotAllowed);
        }
        if host
            .parse::<IpAddr>()
            .ok()
            .is_some_and(is_private_or_reserved)
            && !self.allow_private_network
        {
            return Err(MigrationInputError::PrivateNetworkNotAllowed);
        }
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }
        Ok(url)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MigrationMergePolicy {
    #[default]
    Merge,
    Overwrite,
    Skip,
}

fn default_scope_enabled() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationScope {
    #[serde(default = "default_scope_enabled")]
    pub user_profile: bool,
    #[serde(default = "default_scope_enabled")]
    pub library_access: bool,
    #[serde(default = "default_scope_enabled")]
    pub item_state: bool,
    /// New requests may narrow media-state migration to a subset of source
    /// queries.  `None` preserves the all-state behavior used by jobs created
    /// before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_state_filters: Option<Vec<MigrationUserStateFilter>>,
    #[serde(default = "default_scope_enabled")]
    pub person_favorites: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_library_ids: Option<Vec<String>>,
}

impl Default for MigrationScope {
    fn default() -> Self {
        Self {
            user_profile: true,
            library_access: true,
            item_state: true,
            item_state_filters: None,
            person_favorites: true,
            target_library_ids: None,
        }
    }
}

impl MigrationScope {
    pub fn has_selected_category(&self) -> bool {
        self.user_profile || self.library_access || self.item_state || self.person_favorites
    }

    pub fn requires_target_libraries(&self) -> bool {
        self.library_access || self.item_state
    }

    pub fn selected_item_state_filters(&self) -> &[MigrationUserStateFilter] {
        if !self.item_state {
            return &[];
        }
        self.item_state_filters
            .as_deref()
            .unwrap_or(&MigrationUserStateFilter::ALL)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HistoryCapability {
    ItemState,
    EventHistory,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationConnectionInfo {
    pub server_name: Option<String>,
    pub product_name: Option<String>,
    pub version: Option<String>,
    pub server_id: Option<String>,
    pub history_capability: HistoryCapability,
    /// Newer migration plugins can enforce the selected source-library and
    /// user-data field projection before issuing Emby requests.  Older
    /// plugins omit this field and retain the legacy read behaviour.
    #[serde(default)]
    pub supports_filtered_reads: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationUser {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub has_password: bool,
    #[serde(default)]
    pub is_disabled: bool,
    #[serde(default)]
    pub is_administrator: bool,
    #[serde(default)]
    pub enable_all_folders: bool,
    #[serde(default)]
    pub enabled_folders: Vec<String>,
    #[serde(default)]
    pub enable_remote_access: bool,
    #[serde(default)]
    pub enable_content_downloading: bool,
    pub primary_image_tag: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationLibraryFolder {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub locations: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationUserPage {
    pub items: Vec<MigrationUser>,
    pub history_capability: HistoryCapability,
    #[serde(default)]
    pub library_folders: Option<Vec<MigrationLibraryFolder>>,
    /// Optional pagination metadata returned by newer migration plugins.
    /// Legacy plugins omit these fields and continue to return a complete list.
    #[serde(default)]
    pub start_index: Option<i64>,
    #[serde(default)]
    pub total_record_count: Option<i64>,
    #[serde(default)]
    pub next_start_index: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationItem {
    pub id: String,
    pub name: String,
    pub item_type: String,
    pub production_year: Option<i64>,
    pub provider_ids: std::collections::BTreeMap<String, String>,
    pub parent_id: Option<String>,
    pub series_id: Option<String>,
    pub season_id: Option<String>,
    pub index_number: Option<i64>,
    pub parent_index_number: Option<i64>,
    pub user_data: Option<MigrationUserData>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationUserData {
    #[serde(default)]
    pub playback_position_ticks: i64,
    #[serde(default)]
    pub played: bool,
    #[serde(default)]
    pub is_favorite: bool,
    #[serde(default)]
    pub play_count: i64,
    #[serde(default)]
    pub last_played_date: Option<String>,
}

impl MigrationUserData {
    pub fn has_recorded_state(&self) -> bool {
        self.playback_position_ticks > 0
            || self.played
            || self.is_favorite
            || self.play_count > 0
            || self.last_played_date.is_some()
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MigrationUserStateFilter {
    Played,
    Favorite,
    Resumable,
}

impl MigrationUserStateFilter {
    pub const ALL: [Self; 3] = [Self::Played, Self::Favorite, Self::Resumable];

    pub const fn requested_fields(self) -> &'static [&'static str] {
        match self {
            Self::Played => &["played", "playCount", "lastPlayedDate"],
            Self::Favorite => &["isFavorite"],
            Self::Resumable => &["playbackPositionTicks"],
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MigrationSourceFilter<'a> {
    pub(crate) library_ids: Option<&'a [String]>,
    pub(crate) enabled: bool,
    pub(crate) state_fields: Option<&'a [&'static str]>,
}

impl MigrationSourceFilter<'_> {
    pub(crate) const fn disabled() -> Self {
        Self {
            library_ids: None,
            enabled: false,
            state_fields: None,
        }
    }
}

#[cfg(test)]
mod user_data_tests {
    use super::{
        EmbyMigrationSource, MigrationScope, MigrationSourceFilter, MigrationUser,
        MigrationUserData, MigrationUserStateFilter, migration_list_users_params,
        migration_list_users_params_with, migration_user_state_params,
        requested_fields_for_state_filters,
    };
    use serde_json::json;

    #[test]
    fn migration_scope_defaults_to_all_categories() {
        let scope = MigrationScope::default();
        assert!(scope.user_profile);
        assert!(scope.library_access);
        assert!(scope.item_state);
        assert!(scope.person_favorites);
    }

    #[test]
    fn migration_scope_allows_partial_json_without_disabling_other_categories() {
        let scope: MigrationScope = serde_json::from_str(r#"{"itemState":false}"#)
            .expect("partial migration scope should deserialize");
        assert!(scope.user_profile);
        assert!(scope.library_access);
        assert!(!scope.item_state);
        assert!(scope.person_favorites);
    }

    #[test]
    fn migration_scope_uses_only_explicitly_selected_item_state_filters() {
        let scope: MigrationScope =
            serde_json::from_str(r#"{"itemState":true,"itemStateFilters":["FAVORITE"]}"#)
                .expect("selected item-state filters should deserialize");

        assert_eq!(
            scope.selected_item_state_filters(),
            &[MigrationUserStateFilter::Favorite]
        );
    }

    #[test]
    fn legacy_item_state_scope_keeps_all_state_filters() {
        let scope: MigrationScope = serde_json::from_str(r#"{"itemState":true}"#)
            .expect("legacy item-state scope should deserialize");

        assert_eq!(
            scope.selected_item_state_filters(),
            &MigrationUserStateFilter::ALL
        );
    }

    #[test]
    fn filtered_user_state_request_contains_only_selected_source_scope() {
        let source = EmbyMigrationSource {
            base_url: "http://emby.example/".to_owned(),
            api_key: "redacted-test-key".to_owned(),
            allow_private_network: false,
        };
        let source_library_ids = ["source-library-1".to_owned()];
        let params = migration_user_state_params(
            &source,
            "user-1",
            0,
            500,
            MigrationUserStateFilter::Favorite,
            MigrationSourceFilter {
                library_ids: Some(&source_library_ids),
                enabled: true,
                state_fields: None,
            },
        )
        .expect("valid filtered request");

        assert_eq!(params["stateFilter"], json!("FAVORITE"));
        assert_eq!(params["stateFields"], json!(["isFavorite"]));
        assert_eq!(params["sourceLibraryIds"], json!(["source-library-1"]));
        assert!(params.get("includeItemTypes").is_none());
    }

    #[test]
    fn filtered_user_state_request_projects_the_union_of_selected_fields() {
        let source = EmbyMigrationSource {
            base_url: "http://emby.example/".to_owned(),
            api_key: "redacted-test-key".to_owned(),
            allow_private_network: false,
        };
        let fields = requested_fields_for_state_filters(&[
            MigrationUserStateFilter::Played,
            MigrationUserStateFilter::Favorite,
        ]);
        let params = migration_user_state_params(
            &source,
            "user-1",
            0,
            500,
            MigrationUserStateFilter::Favorite,
            MigrationSourceFilter {
                library_ids: None,
                enabled: true,
                state_fields: Some(&fields),
            },
        )
        .expect("valid union field projection");

        assert_eq!(
            params["stateFields"],
            json!(["played", "playCount", "lastPlayedDate", "isFavorite"])
        );
    }

    #[test]
    fn paged_user_list_request_contains_optional_bounds_and_search() {
        let source = EmbyMigrationSource {
            base_url: "http://emby.example/".to_owned(),
            api_key: "redacted-test-key".to_owned(),
            allow_private_network: false,
        };
        let params = migration_list_users_params(&source, 200, 100, Some(" Alice "))
            .expect("valid paged user request");

        assert_eq!(params["startIndex"], json!(200));
        assert_eq!(params["limit"], json!(100));
        assert_eq!(params["search"], json!("Alice"));
        assert_eq!(
            params["userFields"],
            json!(["id", "name", "isDisabled", "isAdministrator"])
        );
    }

    #[test]
    fn paged_user_list_request_omits_blank_search_and_clamps_limit() {
        let source = EmbyMigrationSource {
            base_url: "http://emby.example/".to_owned(),
            api_key: "redacted-test-key".to_owned(),
            allow_private_network: false,
        };
        let params = migration_list_users_params(&source, 0, 0, Some("  "))
            .expect("valid empty-search request");

        assert_eq!(params["limit"], json!(1));
        assert!(params.get("search").is_none());
    }

    #[test]
    fn filtered_user_list_request_projects_selected_users_and_fields() {
        let source = EmbyMigrationSource {
            base_url: "http://emby.example/".to_owned(),
            api_key: "redacted-test-key".to_owned(),
            allow_private_network: false,
        };
        let user_ids = ["user-1".to_owned(), "user-2".to_owned()];
        let fields = ["id", "name", "hasPassword", "isDisabled"];
        let params = migration_list_users_params_with(
            &source,
            Some(0),
            Some(500),
            None,
            Some(&user_ids),
            Some(&fields),
        )
        .expect("valid filtered user request");

        assert_eq!(params["userIds"], json!(["user-1", "user-2"]));
        assert_eq!(
            params["userFields"],
            json!(["id", "name", "hasPassword", "isDisabled"])
        );
    }

    #[test]
    fn projected_source_user_response_defaults_unrequested_fields() {
        let user: MigrationUser = serde_json::from_value(json!({
            "id": "user-1",
            "name": "Alice"
        }))
        .expect("minimal projected user should deserialize");

        assert_eq!(user.id, "user-1");
        assert_eq!(user.name, "Alice");
        assert!(!user.has_password);
        assert!(!user.is_disabled);
        assert!(!user.enable_all_folders);
        assert!(user.enabled_folders.is_empty());
        assert!(!user.enable_remote_access);
        assert!(!user.enable_content_downloading);
        assert!(user.primary_image_tag.is_none());
    }

    #[test]
    fn partial_source_user_data_defaults_unrequested_fields() {
        let data: MigrationUserData = serde_json::from_value(json!({
            "isFavorite": true
        }))
        .expect("partial state projection should deserialize");

        assert!(data.is_favorite);
        assert_eq!(data.playback_position_ticks, 0);
        assert!(!data.played);
        assert_eq!(data.play_count, 0);
        assert!(data.last_played_date.is_none());
    }

    #[test]
    fn empty_user_data_is_not_a_recorded_state() {
        let data = MigrationUserData {
            playback_position_ticks: 0,
            played: false,
            is_favorite: false,
            play_count: 0,
            last_played_date: None,
        };

        assert!(!data.has_recorded_state());
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationItemPage {
    pub items: Vec<MigrationItem>,
    pub start_index: u32,
    pub total_record_count: Option<u32>,
    pub next_start_index: Option<u32>,
    pub history_capability: HistoryCapability,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationAuthenticatedUser {
    pub authenticated: bool,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredItemState {
    pub position_ticks: i64,
    pub is_played: bool,
    pub is_favorite: bool,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
}

pub fn merge_item_state(
    existing: Option<StoredItemState>,
    incoming: StoredItemState,
    policy: MigrationMergePolicy,
) -> Option<StoredItemState> {
    let Some(existing) = existing else {
        return Some(incoming);
    };
    match policy {
        MigrationMergePolicy::Skip => Some(existing),
        MigrationMergePolicy::Overwrite => Some(incoming),
        MigrationMergePolicy::Merge => {
            let incoming_is_newer = incoming.last_played_at > existing.last_played_at;
            Some(StoredItemState {
                position_ticks: if incoming_is_newer {
                    incoming.position_ticks
                } else if incoming.last_played_at == existing.last_played_at {
                    incoming.position_ticks.max(existing.position_ticks)
                } else {
                    existing.position_ticks
                },
                is_played: existing.is_played || incoming.is_played,
                is_favorite: existing.is_favorite || incoming.is_favorite,
                play_count: existing.play_count.max(incoming.play_count),
                last_played_at: incoming.last_played_at.max(existing.last_played_at),
            })
        }
    }
}

#[derive(Clone)]
pub struct EmbyMigrationPluginClient {
    plugins: PluginService,
}

impl EmbyMigrationPluginClient {
    pub fn new(plugins: PluginService) -> Self {
        Self { plugins }
    }

    pub async fn configured_source(&self) -> Result<EmbyMigrationSource, PluginServiceError> {
        let values = self
            .plugins
            .plugin_config_values(EMBY_MIGRATION_PLUGIN_ID)
            .await?;
        emby_migration_source_from_values(&values)
    }

    pub async fn test_connection(
        &self,
        source: &EmbyMigrationSource,
    ) -> Result<MigrationConnectionInfo, PluginServiceError> {
        source
            .validate()
            .map_err(|_| PluginServiceError::InvalidConfig)?;
        self.call(MIGRATION_TEST_METHOD, source).await
    }

    pub async fn list_users(
        &self,
        source: &EmbyMigrationSource,
    ) -> Result<MigrationUserPage, PluginServiceError> {
        self.list_users_filtered(source, None).await
    }

    pub(crate) async fn list_users_filtered(
        &self,
        source: &EmbyMigrationSource,
        selected_user_ids: Option<&[String]>,
    ) -> Result<MigrationUserPage, PluginServiceError> {
        self.list_users_filtered_with_fields(source, selected_user_ids, None)
            .await
    }

    pub(crate) async fn list_users_filtered_with_fields(
        &self,
        source: &EmbyMigrationSource,
        selected_user_ids: Option<&[String]>,
        user_fields: Option<&[&'static str]>,
    ) -> Result<MigrationUserPage, PluginServiceError> {
        let params = migration_list_users_params_with(
            source,
            None,
            None,
            None,
            selected_user_ids,
            user_fields,
        )?;
        self.call_with(MIGRATION_LIST_USERS_METHOD, params)
            .await
            .map(normalize_migration_user_page)
    }

    /// Read one bounded source-user page.  Pagination and search are optional
    /// protocol fields so older plugins can ignore them and return their
    /// complete user list; the host applies the legacy slice in that case.
    pub(crate) async fn list_users_page(
        &self,
        source: &EmbyMigrationSource,
        start_index: i64,
        limit: i64,
        search: Option<&str>,
    ) -> Result<MigrationUserPage, PluginServiceError> {
        let params = migration_list_users_params(source, start_index, limit, search)?;
        self.call_with(MIGRATION_LIST_USERS_METHOD, params)
            .await
            .map(normalize_migration_user_page)
    }

    pub async fn list_items(
        &self,
        source: &EmbyMigrationSource,
        user_id: &str,
        start_index: u32,
        limit: u32,
    ) -> Result<MigrationItemPage, PluginServiceError> {
        let page = self
            .call_with(
                MIGRATION_LIST_ITEMS_METHOD,
                serde_json::json!({
                    "source": source,
                    "userId": validate_id(user_id)?,
                    "startIndex": start_index,
                    "limit": limit.min(MAX_PAGE_SIZE as u32).max(1),
                }),
            )
            .await?;
        Ok(normalize_migration_item_page(page))
    }

    pub async fn user_state(
        &self,
        source: &EmbyMigrationSource,
        user_id: &str,
        start_index: u32,
        limit: u32,
        state_filter: MigrationUserStateFilter,
    ) -> Result<MigrationItemPage, PluginServiceError> {
        self.user_state_filtered(
            source,
            user_id,
            start_index,
            limit,
            state_filter,
            MigrationSourceFilter::disabled(),
        )
        .await
    }

    pub(crate) async fn user_state_filtered(
        &self,
        source: &EmbyMigrationSource,
        user_id: &str,
        start_index: u32,
        limit: u32,
        state_filter: MigrationUserStateFilter,
        source_filter: MigrationSourceFilter<'_>,
    ) -> Result<MigrationItemPage, PluginServiceError> {
        let params = migration_user_state_params(
            source,
            user_id,
            start_index,
            limit,
            state_filter,
            source_filter,
        )?;
        let page = self.call_with(MIGRATION_USER_STATE_METHOD, params).await?;
        Ok(normalize_migration_item_page(page))
    }

    pub async fn person_favorites(
        &self,
        source: &EmbyMigrationSource,
        user_id: &str,
        start_index: u32,
        limit: u32,
    ) -> Result<MigrationItemPage, PluginServiceError> {
        self.person_favorites_filtered(
            source,
            user_id,
            start_index,
            limit,
            MigrationSourceFilter::disabled(),
        )
        .await
    }

    pub(crate) async fn person_favorites_filtered(
        &self,
        source: &EmbyMigrationSource,
        user_id: &str,
        start_index: u32,
        limit: u32,
        source_filter: MigrationSourceFilter<'_>,
    ) -> Result<MigrationItemPage, PluginServiceError> {
        let mut params = serde_json::json!({
            "source": source,
            "userId": validate_id(user_id)?,
            "startIndex": start_index,
            "limit": limit.min(MAX_PAGE_SIZE as u32).max(1),
        });
        if source_filter.enabled {
            if let Some(source_library_ids) = source_filter.library_ids {
                params["sourceLibraryIds"] = serde_json::json!(source_library_ids);
            }
        }
        let page = self
            .call_with(MIGRATION_PERSON_FAVORITES_METHOD, params)
            .await?;
        Ok(normalize_migration_item_page(page))
    }

    pub async fn authenticate_user(
        &self,
        source: &EmbyMigrationSource,
        username: &str,
        password: &str,
    ) -> Result<MigrationAuthenticatedUser, PluginServiceError> {
        if username.trim().is_empty()
            || username.chars().count() > MAX_TEXT_LENGTH
            || password.is_empty()
            || password.chars().count() > MAX_SECRET_LENGTH
        {
            return Err(PluginServiceError::InvalidConfig);
        }
        self.call_with(
            MIGRATION_AUTHENTICATE_USER_METHOD,
            serde_json::json!({
                "source": source,
                "username": username,
                "password": password,
            }),
        )
        .await
    }

    async fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        source: &EmbyMigrationSource,
    ) -> Result<T, PluginServiceError> {
        source
            .validate()
            .map_err(|_| PluginServiceError::InvalidConfig)?;
        self.call_with(method, serde_json::json!({"source": source}))
            .await
    }

    async fn call_with<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, PluginServiceError> {
        let value = self.plugins.call_migration(method, params).await?;
        serde_json::from_value(value).map_err(|_| PluginServiceError::InvalidResponse)
    }
}

fn normalize_migration_user_page(mut page: MigrationUserPage) -> MigrationUserPage {
    for user in &mut page.items {
        user.name = normalize_migration_text(&user.name);
    }
    page
}

fn migration_user_state_params(
    source: &EmbyMigrationSource,
    user_id: &str,
    start_index: u32,
    limit: u32,
    state_filter: MigrationUserStateFilter,
    source_filter: MigrationSourceFilter<'_>,
) -> Result<Value, PluginServiceError> {
    let mut params = serde_json::json!({
        "source": source,
        "userId": validate_id(user_id)?,
        "startIndex": start_index,
        "limit": limit.min(MAX_PAGE_SIZE as u32).max(1),
        "stateFilter": state_filter,
    });
    if source_filter.enabled {
        params["stateFields"] = serde_json::json!(
            source_filter
                .state_fields
                .unwrap_or_else(|| state_filter.requested_fields())
        );
    }
    if source_filter.enabled {
        if let Some(source_library_ids) = source_filter.library_ids {
            params["sourceLibraryIds"] = serde_json::json!(source_library_ids);
        }
    }
    Ok(params)
}

fn migration_list_users_params(
    source: &EmbyMigrationSource,
    start_index: i64,
    limit: i64,
    search: Option<&str>,
) -> Result<Value, PluginServiceError> {
    migration_list_users_params_with(
        source,
        Some(start_index),
        Some(limit),
        search,
        None,
        Some(&SOURCE_USER_PREVIEW_FIELDS),
    )
}

fn migration_list_users_params_with(
    source: &EmbyMigrationSource,
    start_index: Option<i64>,
    limit: Option<i64>,
    search: Option<&str>,
    selected_user_ids: Option<&[String]>,
    user_fields: Option<&[&'static str]>,
) -> Result<Value, PluginServiceError> {
    source
        .validate()
        .map_err(|_| PluginServiceError::InvalidConfig)?;
    let search = search.unwrap_or_default().trim();
    if search.chars().count() > MAX_TEXT_LENGTH {
        return Err(PluginServiceError::InvalidConfig);
    }
    let mut params = serde_json::json!({"source": source});
    if let Some(start_index) = start_index {
        if start_index < 0 {
            return Err(PluginServiceError::InvalidConfig);
        }
        params["startIndex"] = serde_json::json!(start_index);
    }
    if let Some(limit) = limit {
        params["limit"] = serde_json::json!(limit.clamp(1, MAX_PAGE_SIZE));
    }
    if !search.is_empty() {
        params["search"] = Value::String(search.to_owned());
    }
    if let Some(selected_user_ids) = selected_user_ids {
        let selected_user_ids = selected_user_ids
            .iter()
            .map(|user_id| validate_id(user_id))
            .collect::<Result<Vec<_>, _>>()?;
        params["userIds"] = serde_json::json!(selected_user_ids);
    }
    if let Some(user_fields) = user_fields.filter(|fields| !fields.is_empty()) {
        params["userFields"] = serde_json::json!(user_fields);
    }
    Ok(params)
}

pub(crate) fn requested_fields_for_state_filters(
    filters: &[MigrationUserStateFilter],
) -> Vec<&'static str> {
    let mut fields = Vec::new();
    for filter in filters {
        for field in filter.requested_fields() {
            if !fields.contains(field) {
                fields.push(*field);
            }
        }
    }
    fields
}

fn normalize_migration_item_page(mut page: MigrationItemPage) -> MigrationItemPage {
    for item in &mut page.items {
        item.name = normalize_migration_text(&item.name);
    }
    page
}

fn normalize_migration_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn emby_migration_source_from_values(
    values: &serde_json::Map<String, Value>,
) -> Result<EmbyMigrationSource, PluginServiceError> {
    let source = EmbyMigrationSource {
        base_url: values
            .get("baseUrl")
            .and_then(Value::as_str)
            .ok_or(PluginServiceError::InvalidConfig)?
            .trim()
            .to_owned(),
        api_key: values
            .get("apiKey")
            .and_then(Value::as_str)
            .ok_or(PluginServiceError::InvalidConfig)?
            .trim()
            .to_owned(),
        allow_private_network: values
            .get("allowPrivateNetwork")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    };
    source
        .validate()
        .map_err(|_| PluginServiceError::InvalidConfig)?;
    Ok(source)
}

fn validate_id(value: &str) -> Result<String, PluginServiceError> {
    if value.is_empty()
        || value.len() > MAX_ID_LENGTH
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(PluginServiceError::InvalidConfig);
    }
    Ok(value.to_owned())
}

fn is_private_name(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".home.arpa")
}

fn is_private_or_reserved(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(value) => {
            value.is_private()
                || value.is_loopback()
                || value.is_link_local()
                || value.is_unspecified()
                || value.is_multicast()
        }
        IpAddr::V6(value) => {
            value.is_loopback()
                || value.is_unspecified()
                || value.is_multicast()
                || value.is_unicast_link_local()
                || (value.octets()[0] & 0xfe) == 0xfc
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_text_normalization_replaces_control_characters() {
        assert_eq!(
            normalize_migration_text("  现在\t要做\n决定\u{0000}  "),
            "现在 要做 决定"
        );
    }

    #[test]
    fn configured_source_uses_plugin_values_and_private_network_flag() {
        let source = emby_migration_source_from_values(
            serde_json::json!({
                "baseUrl": "http://emby.local:8096",
                "apiKey": "secret",
                "allowPrivateNetwork": true,
            })
            .as_object()
            .expect("object"),
        )
        .expect("configured source should validate");

        assert_eq!(source.base_url, "http://emby.local:8096");
        assert_eq!(source.api_key, "secret");
        assert!(source.allow_private_network);
    }

    #[test]
    fn configured_source_rejects_missing_plugin_credentials() {
        let error = emby_migration_source_from_values(
            serde_json::json!({
                "baseUrl": "http://emby.local:8096",
            })
            .as_object()
            .expect("object"),
        )
        .expect_err("missing API key should be invalid");

        assert!(matches!(error, PluginServiceError::InvalidConfig));
    }

    #[test]
    fn source_validation_rejects_credentials_and_query_secrets() {
        for base_url in [
            "https://user:pass@emby.example.test",
            "https://emby.example.test?api_key=secret",
            "https://emby.example.test/#fragment",
        ] {
            let source = EmbyMigrationSource {
                base_url: base_url.to_owned(),
                api_key: "api-key".to_owned(),
                allow_private_network: false,
            };
            assert_eq!(
                source.validate(),
                Err(MigrationInputError::InvalidSourceUrl)
            );
        }
    }

    #[test]
    fn merge_keeps_newer_progress_and_never_loses_favorite() {
        let existing = StoredItemState {
            position_ticks: 300,
            is_played: false,
            is_favorite: true,
            play_count: 3,
            last_played_at: Some(100),
        };
        let incoming = StoredItemState {
            position_ticks: 500,
            is_played: true,
            is_favorite: false,
            play_count: 2,
            last_played_at: Some(200),
        };
        assert_eq!(
            merge_item_state(Some(existing), incoming, MigrationMergePolicy::Merge),
            Some(StoredItemState {
                position_ticks: 500,
                is_played: true,
                is_favorite: true,
                play_count: 3,
                last_played_at: Some(200),
            })
        );
    }

    #[test]
    fn merge_uses_the_newer_playback_position_even_when_it_is_lower() {
        let existing = StoredItemState {
            position_ticks: 900,
            is_played: false,
            is_favorite: false,
            play_count: 1,
            last_played_at: Some(100),
        };
        let incoming = StoredItemState {
            position_ticks: 120,
            is_played: false,
            is_favorite: false,
            play_count: 1,
            last_played_at: Some(200),
        };
        assert_eq!(
            merge_item_state(Some(existing), incoming, MigrationMergePolicy::Merge)
                .expect("merge should return a state")
                .position_ticks,
            120
        );
    }
}
