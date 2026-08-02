use crate::{
    auth::password::{PasswordError, PasswordService},
    domain::ids::UserId,
    storage::{Database, StorageError},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserRecord {
    pub id: UserId,
    pub username_normalized: String,
    pub display_name: String,
    pub is_disabled: bool,
    pub is_admin: bool,
    pub can_manage_server: bool,
    pub can_remote_access: bool,
    pub can_download: bool,
}

#[derive(Clone)]
pub struct UserStore {
    database: Database,
    passwords: PasswordService,
}

impl UserStore {
    pub fn new(database: Database) -> Result<Self, PasswordError> {
        Ok(Self {
            database,
            passwords: PasswordService::new()?,
        })
    }

    pub async fn create_user(
        &self,
        username: &str,
        display_name: &str,
        password: &str,
        is_admin: bool,
    ) -> Result<UserRecord, UserStoreError> {
        let username_normalized = normalize_username(username)?;
        let display_name = normalized_display_name(display_name, &username_normalized);
        let password_hash = self.passwords.hash_password(password)?;
        let id = UserId::new();

        self.database
            .insert_user(
                &id.to_string(),
                &username_normalized,
                &display_name,
                &password_hash,
                is_admin,
            )
            .await?;

        Ok(UserRecord {
            id,
            username_normalized,
            display_name,
            is_disabled: false,
            is_admin,
            can_manage_server: is_admin,
            can_remote_access: false,
            can_download: false,
        })
    }

    pub async fn has_users(&self) -> Result<bool, UserStoreError> {
        Ok(self.database.has_users().await?)
    }

    pub async fn create_initial_admin(
        &self,
        username: &str,
        display_name: &str,
        password: &str,
    ) -> Result<UserRecord, UserStoreError> {
        let username_normalized = normalize_username(username)?;
        let display_name = normalized_display_name(display_name, &username_normalized);
        let password_hash = self.passwords.hash_password(password)?;
        let id = UserId::new();
        let inserted = self
            .database
            .insert_initial_user(
                &id.to_string(),
                &username_normalized,
                &display_name,
                &password_hash,
            )
            .await?;
        if !inserted {
            return Err(UserStoreError::SetupAlreadyCompleted);
        }

        Ok(UserRecord {
            id,
            username_normalized,
            display_name,
            is_disabled: false,
            is_admin: true,
            can_manage_server: true,
            can_remote_access: false,
            can_download: false,
        })
    }

    pub async fn authenticate(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Option<UserRecord>, UserStoreError> {
        let username_normalized = normalize_username(username)?;
        let stored = self
            .database
            .find_user_by_username(&username_normalized)
            .await?;
        let stored_hash = stored.as_ref().map(|user| user.password_hash.as_str());
        let password_matches = self.passwords.verify_password(stored_hash, password)?;

        let Some(stored) = stored else {
            return Ok(None);
        };
        if !password_matches || stored.is_disabled {
            return Ok(None);
        }

        let id = stored
            .id
            .parse()
            .map_err(|error: uuid::Error| UserStoreError::InvalidUserId(error.to_string()))?;
        Ok(Some(UserRecord {
            id,
            username_normalized: stored.username_normalized,
            display_name: stored.display_name,
            is_disabled: stored.is_disabled,
            is_admin: stored.is_admin,
            can_manage_server: stored.can_manage_server,
            can_remote_access: stored.can_remote_access,
            can_download: stored.can_download,
        }))
    }
}

fn normalize_username(username: &str) -> Result<String, UserStoreError> {
    let normalized = username.trim().to_lowercase();
    if normalized.is_empty() || normalized.chars().count() > 128 {
        return Err(UserStoreError::InvalidUsername);
    }
    Ok(normalized)
}

fn normalized_display_name(display_name: &str, username_normalized: &str) -> String {
    let display_name = display_name.trim();
    if display_name.is_empty() {
        username_normalized.to_owned()
    } else {
        display_name.to_owned()
    }
}

#[derive(Debug)]
pub enum UserStoreError {
    InvalidUsername,
    InvalidUserId(String),
    SetupAlreadyCompleted,
    Password(PasswordError),
    Storage(StorageError),
}

impl From<PasswordError> for UserStoreError {
    fn from(error: PasswordError) -> Self {
        Self::Password(error)
    }
}

impl From<StorageError> for UserStoreError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl std::fmt::Display for UserStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUsername => formatter.write_str("username must be 1-128 characters"),
            Self::InvalidUserId(error) => write!(formatter, "stored user ID is invalid: {error}"),
            Self::SetupAlreadyCompleted => {
                formatter.write_str("initial setup has already completed")
            }
            Self::Password(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for UserStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Password(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::InvalidUsername | Self::InvalidUserId(_) | Self::SetupAlreadyCompleted => None,
        }
    }
}
