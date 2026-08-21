use std::{fmt, net::IpAddr};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

use super::{
    plugin_protocol::{
        MIGRATION_AUTHENTICATE_USER_METHOD, MIGRATION_LIST_ITEMS_METHOD,
        MIGRATION_LIST_USERS_METHOD, MIGRATION_TEST_METHOD, MIGRATION_USER_STATE_METHOD,
    },
    plugins::{PluginService, PluginServiceError},
};

const MAX_SOURCE_URL_LENGTH: usize = 2048;
const MAX_SECRET_LENGTH: usize = 1024;
const MAX_TEXT_LENGTH: usize = 1024;
const MAX_ID_LENGTH: usize = 256;
const MAX_PAGE_SIZE: i64 = 500;

#[derive(Debug, Eq, PartialEq)]
pub enum MigrationInputError {
    InvalidSourceUrl,
    PrivateNetworkNotAllowed,
    InvalidSecret,
    InvalidIdentifier,
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
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationUser {
    pub id: String,
    pub name: String,
    pub has_password: bool,
    pub is_disabled: bool,
    pub is_administrator: bool,
    pub enable_all_folders: bool,
    pub enabled_folders: Vec<String>,
    pub primary_image_tag: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationUserPage {
    pub items: Vec<MigrationUser>,
    pub history_capability: HistoryCapability,
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
    pub playback_position_ticks: i64,
    pub played: bool,
    pub is_favorite: bool,
    pub play_count: i64,
    pub last_played_date: Option<String>,
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
        self.call(MIGRATION_LIST_USERS_METHOD, source).await
    }

    pub async fn list_items(
        &self,
        source: &EmbyMigrationSource,
        user_id: &str,
        start_index: u32,
        limit: u32,
    ) -> Result<MigrationItemPage, PluginServiceError> {
        self.call_with(
            MIGRATION_LIST_ITEMS_METHOD,
            serde_json::json!({
                "source": source,
                "userId": validate_id(user_id)?,
                "startIndex": start_index,
                "limit": limit.min(MAX_PAGE_SIZE as u32).max(1),
            }),
        )
        .await
    }

    pub async fn user_state(
        &self,
        source: &EmbyMigrationSource,
        user_id: &str,
        start_index: u32,
        limit: u32,
    ) -> Result<MigrationItemPage, PluginServiceError> {
        self.call_with(
            MIGRATION_USER_STATE_METHOD,
            serde_json::json!({
                "source": source,
                "userId": validate_id(user_id)?,
                "startIndex": start_index,
                "limit": limit.min(MAX_PAGE_SIZE as u32).max(1),
            }),
        )
        .await
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
