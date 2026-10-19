use std::fmt;

use crate::{
    domain::{
        content_restrictions::{
            ContentRestrictionClassification, ContentRestrictionPolicy,
            content_classification_is_allowed, content_restriction_policy_is_restrictive,
        },
        ids::UserId,
    },
    storage::{Database, GLOBAL_COLLECTION_LIBRARY_ID, StorageError, StoredPlaybackSource},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccessPrincipal {
    pub user_id: Option<UserId>,
    pub is_admin: bool,
}

impl AccessPrincipal {
    pub const fn new(user_id: UserId, is_admin: bool) -> Self {
        Self {
            user_id: Some(user_id),
            is_admin,
        }
    }

    pub const fn server_admin(target_user_id: Option<UserId>) -> Self {
        Self {
            user_id: target_user_id,
            is_admin: true,
        }
    }

    pub fn user_id_string(self) -> String {
        self.user_id.map_or_else(String::new, |id| id.to_string())
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
        if library_id == GLOBAL_COLLECTION_LIBRARY_ID {
            return Ok(false);
        }
        if principal.is_admin {
            return Ok(true);
        }
        let Some(user_id) = principal.user_id else {
            return Ok(false);
        };
        Ok(self
            .database
            .has_user_library_access(&user_id.to_string(), library_id)
            .await?)
    }

    pub async fn can_view_item(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
    ) -> Result<bool, AccessError> {
        let Some(library_id) = self.database.find_item_library_id(item_id).await? else {
            return Ok(false);
        };
        if library_id == GLOBAL_COLLECTION_LIBRARY_ID {
            return Ok(false);
        }
        if principal.is_admin {
            return Ok(true);
        }
        if !self.can_view_library(principal, &library_id).await? {
            return Ok(false);
        }
        let user_id = principal.user_id_string();
        let Some(restrictions) = self.database.user_content_restrictions(&user_id).await? else {
            return Ok(true);
        };
        let policy = ContentRestrictionPolicy {
            rating_system: &restrictions.rating_system,
            maximum_rating: restrictions.maximum_rating.as_deref(),
            allow_unrated: restrictions.allow_unrated,
            allowed_tags: &restrictions.allowed_tags,
            blocked_tags: &restrictions.blocked_tags,
        };
        if !content_restriction_policy_is_restrictive(&policy) {
            return Ok(true);
        }
        let classifications = self
            .database
            .media_content_restriction_classifications_for_item(item_id)
            .await?;
        if classifications.is_empty() {
            return Ok(false);
        }
        Ok(classifications
            .into_iter()
            .all(|(certification_code, tags, indexed)| {
                content_classification_is_allowed(
                    &policy,
                    &ContentRestrictionClassification {
                        certification_code: certification_code.as_deref(),
                        tags: &tags,
                        indexed,
                    },
                )
            }))
    }

    pub async fn accessible_library_ids(
        &self,
        principal: AccessPrincipal,
    ) -> Result<Vec<String>, AccessError> {
        if principal.is_admin {
            return Ok(self.database.list_enabled_media_library_ids().await?);
        }
        let Some(user_id) = principal.user_id else {
            return Ok(Vec::new());
        };
        Ok(self
            .database
            .list_accessible_library_ids(&user_id.to_string())
            .await?)
    }

    pub(crate) async fn authorized_playback_source(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<StoredPlaybackSource>, AccessError> {
        Ok(self
            .database
            .find_authorized_playback_source(
                item_id,
                source_id,
                &principal.user_id_string(),
                principal.is_admin,
            )
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

#[cfg(test)]
mod tests {
    use crate::{
        application::libraries::LibraryService,
        auth::users::UserStore,
        config::Config,
        library::LibraryKind,
        storage::{Database, NewUserContentRestrictions, UserContentRestrictionsCasResult},
    };

    use super::{AccessPrincipal, MediaAccessService};

    #[tokio::test]
    async fn content_policy_denies_direct_item_access_and_inherits_parent_restrictions()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let user = UserStore::new(database.clone())?
            .create_user("direct-policy-viewer", "Viewer", "test password", false)
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Direct policy series", LibraryKind::Series, false)
            .await?;
        let user_id = user.id.to_string();
        let library_id = library.id.to_string();
        database
            .set_user_library_access(&user_id, &library_id, true)
            .await?;
        for (item_id, item_type, parent_id, series_id, certification) in [
            ("policy-series", "SERIES", None, None, "R"),
            (
                "policy-season",
                "SEASON",
                Some("policy-series"),
                Some("policy-series"),
                "G",
            ),
            (
                "policy-episode",
                "EPISODE",
                Some("policy-season"),
                Some("policy-series"),
                "G",
            ),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, parent_id, series_id, title,
                     sort_title, identification_status, has_available_source
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_type)
            .bind(parent_id)
            .bind(series_id)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO media_content_restriction_index (item_id, certification_code)
                 VALUES (?, ?)",
            )
            .bind(item_id)
            .bind(certification)
            .execute(database.pool())
            .await?;
        }
        sqlx::query(
            "INSERT INTO media_content_restriction_tags (item_id, tag)
             VALUES ('policy-episode', 'violent')",
        )
        .execute(database.pool())
        .await?;

        let access = MediaAccessService::new(database.clone());
        let principal = AccessPrincipal::new(user.id, false);
        assert!(access.can_view_item(principal, "policy-episode").await?);

        let initial = database
            .compare_and_swap_user_content_restrictions(
                &user_id,
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &[],
                    blocked_tags: &[],
                },
            )
            .await?;
        assert!(matches!(
            initial,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        assert!(!access.can_view_item(principal, "policy-episode").await?);

        sqlx::query(
            "UPDATE media_content_restriction_index
             SET certification_code = 'PG' WHERE item_id = 'policy-series'",
        )
        .execute(database.pool())
        .await?;
        assert!(access.can_view_item(principal, "policy-episode").await?);
        let tag_restriction = database
            .compare_and_swap_user_content_restrictions(
                &user_id,
                1,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &[],
                    blocked_tags: &["violent".to_owned()],
                },
            )
            .await?;
        assert!(matches!(
            tag_restriction,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        assert!(!access.can_view_item(principal, "policy-episode").await?);

        sqlx::query("DELETE FROM media_content_restriction_index WHERE item_id = 'policy-series'")
            .execute(database.pool())
            .await?;
        assert!(!access.can_view_item(principal, "policy-episode").await?);
        Ok(())
    }
}
