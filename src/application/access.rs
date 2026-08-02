use std::fmt;

use crate::{
    domain::ids::UserId,
    storage::{Database, StorageError},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccessPrincipal {
    pub user_id: UserId,
    pub is_admin: bool,
}

impl AccessPrincipal {
    pub const fn new(user_id: UserId, is_admin: bool) -> Self {
        Self { user_id, is_admin }
    }
}

#[derive(Clone)]
pub struct MediaAccessService {
    database: Database,
}

impl MediaAccessService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn can_view_library(
        &self,
        principal: AccessPrincipal,
        library_id: &str,
    ) -> Result<bool, AccessError> {
        if principal.is_admin {
            return Ok(true);
        }
        Ok(self
            .database
            .has_user_library_access(&principal.user_id.to_string(), library_id)
            .await?)
    }

    pub async fn can_view_item(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
    ) -> Result<bool, AccessError> {
        if principal.is_admin {
            return Ok(self.database.find_item_library_id(item_id).await?.is_some());
        }
        let Some(library_id) = self.database.find_item_library_id(item_id).await? else {
            return Ok(false);
        };
        self.can_view_library(principal, &library_id).await
    }

    pub async fn accessible_library_ids(
        &self,
        principal: AccessPrincipal,
    ) -> Result<Vec<String>, AccessError> {
        if principal.is_admin {
            return Ok(self.database.list_enabled_library_ids().await?);
        }
        Ok(self
            .database
            .list_accessible_library_ids(&principal.user_id.to_string())
            .await?)
    }
}

#[derive(Debug)]
pub enum AccessError {
    Storage(StorageError),
}

impl fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AccessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
        }
    }
}

impl From<StorageError> for AccessError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
