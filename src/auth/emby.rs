use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use crate::{
    auth::{
        password::PasswordError,
        users::{UserRecord, UserStore, UserStoreError},
    },
    domain::ids::UserId,
    storage::{Database, NewAccessToken, StorageError, StoredEmbyLoginSessionSummary},
};

#[derive(Clone)]
pub struct EmbyAuthService {
    database: Database,
    users: UserStore,
}

impl EmbyAuthService {
    pub fn new(database: Database) -> Result<Self, PasswordError> {
        Ok(Self {
            users: UserStore::new(database.clone())?,
            database,
        })
    }

    pub async fn public_users(&self) -> Result<Vec<UserRecord>, EmbyAuthError> {
        let stored_users = self.database.list_users().await?;
        stored_users.into_iter().map(user_record).collect()
    }

    pub async fn query_users(
        &self,
        is_disabled: Option<bool>,
        name_starts_with_or_greater: Option<&str>,
        descending: bool,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<UserRecord>, i64), EmbyAuthError> {
        let (stored_users, total_count) = self
            .database
            .query_users(
                is_disabled,
                name_starts_with_or_greater,
                descending,
                offset,
                limit,
            )
            .await?;
        let users = stored_users
            .into_iter()
            .map(user_record)
            .collect::<Result<Vec<_>, _>>()?;
        Ok((users, total_count))
    }

    pub async fn user_by_id(&self, user_id: &str) -> Result<Option<UserRecord>, EmbyAuthError> {
        Ok(self.users.find_by_id(user_id).await?)
    }

    pub async fn authenticate(
        &self,
        username: &str,
        password: &str,
        device: &EmbyDeviceInfo,
    ) -> Result<Option<EmbyAuthResult>, EmbyAuthError> {
        let Some(user) = self
            .users
            .authenticate_emby_name(username, password)
            .await?
        else {
            return Ok(None);
        };
        self.create_access_token(user, device).await.map(Some)
    }

    pub async fn find_failed_authentication_user_id(
        &self,
        username: &str,
    ) -> Result<Option<String>, EmbyAuthError> {
        Ok(self
            .users
            .find_emby_authentication_target_id(username)
            .await?)
    }

    pub async fn authenticate_user_id(
        &self,
        user_id: &str,
        password: &str,
        device: &EmbyDeviceInfo,
    ) -> Result<Option<EmbyAuthResult>, EmbyAuthError> {
        let Some(user) = self.users.authenticate_by_id(user_id, password).await? else {
            return Ok(None);
        };
        self.create_access_token(user, device).await.map(Some)
    }

    async fn create_access_token(
        &self,
        user: UserRecord,
        device: &EmbyDeviceInfo,
    ) -> Result<EmbyAuthResult, EmbyAuthError> {
        let token = generate_token()?;
        let token_id = UserId::new().to_string();
        let token_hash = hash_token(&token);
        let user_id = user.id.to_string();
        self.database
            .create_access_token(NewAccessToken {
                id: &token_id,
                token_hash: &token_hash,
                user_id: &user_id,
                device_id: &device.device_id,
                client_name: &device.client,
                device_name: &device.device,
                client_version: &device.version,
                device_type: None,
            })
            .await?;
        Ok(EmbyAuthResult {
            token,
            session_id: token_id,
            user: user.clone(),
            device: device.clone(),
        })
    }

    pub async fn logout(&self, token: &str) -> Result<(), EmbyAuthError> {
        self.database
            .revoke_access_token(&hash_token(token))
            .await?;
        Ok(())
    }

    pub async fn list_admin_login_sessions(
        &self,
        user_id: &str,
        page_size: i64,
        offset: i64,
    ) -> Result<(Vec<EmbyLoginSessionSummary>, i64), EmbyAuthError> {
        let (sessions, total_count) = self
            .database
            .list_user_emby_login_sessions_for_admin(user_id, page_size, offset)
            .await?;
        Ok((
            sessions
                .into_iter()
                .map(EmbyLoginSessionSummary::from)
                .collect(),
            total_count,
        ))
    }

    pub async fn revoke_admin_login_session(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<bool, EmbyAuthError> {
        Ok(self
            .database
            .revoke_user_emby_login_session_for_admin(user_id, session_id)
            .await?)
    }

    pub async fn revoke_all_admin_login_sessions(
        &self,
        user_id: &str,
    ) -> Result<u64, EmbyAuthError> {
        Ok(self
            .database
            .revoke_all_user_emby_login_sessions_for_admin(user_id)
            .await?)
    }

    pub async fn verify_token(&self, token: &str) -> Result<bool, EmbyAuthError> {
        Ok(self
            .database
            .has_valid_access_token(&hash_token(token))
            .await?)
    }

    pub async fn device_info(&self, token: &str) -> Result<Option<EmbyDeviceInfo>, EmbyAuthError> {
        Ok(self
            .database
            .find_access_token_device(&hash_token(token))
            .await?
            .map(|device| EmbyDeviceInfo {
                client: device.client_name,
                device: device.device_name,
                device_id: device.device_id,
                version: device.client_version,
                user_id: None,
            }))
    }

    pub async fn resolve_token(&self, token: &str) -> Result<Option<UserRecord>, EmbyAuthError> {
        let token_hash = hash_token(token);
        self.database.touch_access_token(&token_hash).await?;
        let Some(stored) = self.database.find_user_by_access_token(&token_hash).await? else {
            return Ok(None);
        };
        if stored.is_disabled {
            return Ok(None);
        }
        user_record(stored).map(Some)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EmbyLoginSessionSummary {
    pub id: String,
    pub device_id: String,
    pub device_name: String,
    pub device_type: Option<String>,
    pub client_name: String,
    pub client_version: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_seen_at: Option<i64>,
}

impl From<StoredEmbyLoginSessionSummary> for EmbyLoginSessionSummary {
    fn from(session: StoredEmbyLoginSessionSummary) -> Self {
        Self {
            id: session.id,
            device_id: session.device_id,
            device_name: session.device_name,
            device_type: session.device_type,
            client_name: session.client_name,
            client_version: session.client_version,
            created_at: session.created_at,
            updated_at: session.updated_at,
            last_seen_at: session.last_seen_at,
        }
    }
}

fn user_record(stored: crate::storage::StoredUser) -> Result<UserRecord, EmbyAuthError> {
    let id = stored
        .id
        .parse()
        .map_err(|error: uuid::Error| EmbyAuthError::InvalidUserId(error.to_string()))?;
    Ok(UserRecord {
        id,
        username_normalized: stored.username_normalized,
        display_name: stored.display_name,
        has_password: stored.has_password,
        is_disabled: stored.is_disabled,
        is_admin: stored.is_admin,
        can_manage_server: stored.can_manage_server,
        can_remote_access: stored.can_remote_access,
        can_download: stored.can_download,
        last_login_at: stored.last_login_at,
        last_activity_at: stored.last_activity_at,
    })
}

#[derive(Clone, Debug, Default)]
pub struct EmbyDeviceInfo {
    pub client: String,
    pub device: String,
    pub device_id: String,
    pub version: String,
    pub user_id: Option<String>,
}

impl EmbyDeviceInfo {
    pub fn parse(header: &str) -> Self {
        let mut info = Self::default();
        let Some((_, values)) = header.split_once(' ') else {
            return info;
        };
        for part in values.split(',') {
            let Some((key, value)) = part.trim().split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"').to_owned();
            match key.trim() {
                "Client" => info.client = value,
                "Device" => info.device = value,
                "DeviceId" => info.device_id = value,
                "Version" => info.version = value,
                "UserId" => info.user_id = Some(value),
                _ => {}
            }
        }
        info
    }
}

#[derive(Clone, Debug)]
pub struct EmbyAuthResult {
    pub token: String,
    pub session_id: String,
    pub user: UserRecord,
    pub device: EmbyDeviceInfo,
}

fn generate_token() -> Result<String, EmbyAuthError> {
    let mut bytes = [0_u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| EmbyAuthError::TokenGeneration(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

#[derive(Debug)]
pub enum EmbyAuthError {
    InvalidUserId(String),
    Password(PasswordError),
    Storage(StorageError),
    TokenGeneration(String),
    UserStore(UserStoreError),
}

impl From<PasswordError> for EmbyAuthError {
    fn from(error: PasswordError) -> Self {
        Self::Password(error)
    }
}

impl From<StorageError> for EmbyAuthError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<UserStoreError> for EmbyAuthError {
    fn from(error: UserStoreError) -> Self {
        Self::UserStore(error)
    }
}

impl std::fmt::Display for EmbyAuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUserId(error) => write!(formatter, "stored user ID is invalid: {error}"),
            Self::Password(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::TokenGeneration(error) => write!(formatter, "token generation failed: {error}"),
            Self::UserStore(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for EmbyAuthError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Password(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::UserStore(error) => Some(error),
            Self::InvalidUserId(_) | Self::TokenGeneration(_) => None,
        }
    }
}
