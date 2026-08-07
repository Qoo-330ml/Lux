use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use crate::{
    auth::{
        password::PasswordError,
        users::{UserRecord, UserStore, UserStoreError},
    },
    domain::ids::UserId,
    storage::{Database, NewAccessToken, StorageError},
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

    pub async fn authenticate(
        &self,
        username: &str,
        password: &str,
        device: &EmbyDeviceInfo,
    ) -> Result<Option<EmbyAuthResult>, EmbyAuthError> {
        let Some(user) = self.users.authenticate(username, password).await? else {
            return Ok(None);
        };
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
            })
            .await?;
        Ok(Some(EmbyAuthResult {
            token,
            session_id: token_id,
            user: user.clone(),
            device: device.clone(),
        }))
    }

    pub async fn logout(&self, token: &str) -> Result<(), EmbyAuthError> {
        self.database
            .revoke_access_token(&hash_token(token))
            .await?;
        Ok(())
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
        let Some(stored) = self
            .database
            .find_user_by_access_token(&hash_token(token))
            .await?
        else {
            return Ok(None);
        };
        if stored.is_disabled {
            return Ok(None);
        }
        user_record(stored).map(Some)
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
        is_disabled: stored.is_disabled,
        is_admin: stored.is_admin,
        can_manage_server: stored.can_manage_server,
        can_remote_access: stored.can_remote_access,
        can_download: stored.can_download,
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
