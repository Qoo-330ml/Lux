use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{
    auth::{
        password::PasswordError,
        users::{UserRecord, UserStore, UserStoreError},
    },
    domain::ids::UserId,
    storage::{Database, StorageError},
};

const SESSION_LIFETIME_SECONDS: i64 = 30 * 24 * 60 * 60;

#[derive(Clone)]
pub struct WebAuthService {
    database: Database,
    users: UserStore,
}

impl WebAuthService {
    pub fn new(database: Database) -> Result<Self, PasswordError> {
        Ok(Self {
            users: UserStore::new(database.clone())?,
            database,
        })
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<LoginSession>, WebAuthError> {
        let Some(user) = self.users.authenticate(username, password).await? else {
            return Ok(None);
        };
        let session_token = generate_token()?;
        let csrf_token = generate_token()?;
        let session_id = UserId::new();
        self.database
            .create_web_session(
                &session_id.to_string(),
                &user.id.to_string(),
                &hash_token(&session_token),
                &hash_token(&csrf_token),
                SESSION_LIFETIME_SECONDS,
            )
            .await?;

        Ok(Some(LoginSession {
            session_token,
            csrf_token,
            user,
        }))
    }

    pub async fn resolve(
        &self,
        session_token: &str,
    ) -> Result<Option<AuthenticatedSession>, WebAuthError> {
        let Some(stored) = self
            .database
            .find_web_session(&hash_token(session_token))
            .await?
        else {
            return Ok(None);
        };
        if stored.is_disabled {
            return Ok(None);
        }
        let id = stored
            .user_id
            .parse()
            .map_err(|error: uuid::Error| WebAuthError::InvalidUserId(error.to_string()))?;
        Ok(Some(AuthenticatedSession {
            user: UserRecord {
                id,
                username_normalized: stored.username_normalized,
                display_name: stored.display_name,
                is_disabled: stored.is_disabled,
                is_admin: stored.is_admin,
                can_manage_server: stored.can_manage_server,
                can_remote_access: stored.can_remote_access,
                can_download: stored.can_download,
            },
            csrf_token_hash: stored.csrf_token_hash,
        }))
    }

    pub async fn logout(&self, session_token: &str) -> Result<(), WebAuthError> {
        self.database
            .revoke_web_session(&hash_token(session_token))
            .await?;
        Ok(())
    }

    pub async fn list_sessions(
        &self,
        user_id: &UserId,
        current_session_token: &str,
    ) -> Result<Vec<WebSessionSummary>, WebAuthError> {
        Ok(self
            .database
            .list_web_session_summaries(&user_id.to_string(), &hash_token(current_session_token))
            .await?
            .into_iter()
            .map(|session| WebSessionSummary {
                id: session.id,
                created_at: session.created_at,
                updated_at: session.updated_at,
                expires_at: session.expires_at,
                last_seen_at: session.last_seen_at,
                is_current: session.is_current,
            })
            .collect())
    }

    pub async fn revoke_session(
        &self,
        user_id: &UserId,
        session_id: &str,
    ) -> Result<bool, WebAuthError> {
        Ok(self
            .database
            .revoke_web_session_by_id(&user_id.to_string(), session_id)
            .await?)
    }

    pub fn verify_csrf(&self, session: &AuthenticatedSession, csrf_token: &str) -> bool {
        hash_token(csrf_token)
            .as_slice()
            .ct_eq(&session.csrf_token_hash)
            .into()
    }
}

#[derive(Clone, Debug)]
pub struct LoginSession {
    pub session_token: String,
    pub csrf_token: String,
    pub user: UserRecord,
}

#[derive(Clone, Debug)]
pub struct WebSessionSummary {
    pub id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
    pub last_seen_at: Option<i64>,
    pub is_current: bool,
}

#[derive(Clone, Debug)]
pub struct AuthenticatedSession {
    pub user: UserRecord,
    csrf_token_hash: Vec<u8>,
}

fn generate_token() -> Result<String, WebAuthError> {
    let mut bytes = [0_u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| WebAuthError::TokenGeneration(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

#[derive(Debug)]
pub enum WebAuthError {
    InvalidUserId(String),
    Password(PasswordError),
    Storage(StorageError),
    TokenGeneration(String),
    UserStore(UserStoreError),
}

impl From<PasswordError> for WebAuthError {
    fn from(error: PasswordError) -> Self {
        Self::Password(error)
    }
}

impl From<StorageError> for WebAuthError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<UserStoreError> for WebAuthError {
    fn from(error: UserStoreError) -> Self {
        Self::UserStore(error)
    }
}

impl std::fmt::Display for WebAuthError {
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

impl std::error::Error for WebAuthError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Password(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::UserStore(error) => Some(error),
            Self::InvalidUserId(_) | Self::TokenGeneration(_) => None,
        }
    }
}
