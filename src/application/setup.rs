use crate::auth::users::{UserRecord, UserStore, UserStoreError};

#[derive(Clone)]
pub struct SetupService {
    users: UserStore,
}

impl SetupService {
    pub fn new(
        database: crate::storage::Database,
    ) -> Result<Self, crate::auth::password::PasswordError> {
        Ok(Self {
            users: UserStore::new(database)?,
        })
    }

    pub async fn status(&self) -> Result<bool, SetupError> {
        Ok(self.users.has_users().await?)
    }

    pub async fn complete(
        &self,
        username: &str,
        display_name: &str,
        password: &str,
    ) -> Result<UserRecord, SetupError> {
        self.users
            .create_initial_admin(username, display_name, password)
            .await
            .map_err(SetupError::from)
    }
}

#[derive(Debug)]
pub enum SetupError {
    AlreadyCompleted,
    UserStore(UserStoreError),
}

impl From<UserStoreError> for SetupError {
    fn from(error: UserStoreError) -> Self {
        match error {
            UserStoreError::SetupAlreadyCompleted => Self::AlreadyCompleted,
            error => Self::UserStore(error),
        }
    }
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyCompleted => formatter.write_str("initial setup has already completed"),
            Self::UserStore(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for SetupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AlreadyCompleted => None,
            Self::UserStore(error) => Some(error),
        }
    }
}
