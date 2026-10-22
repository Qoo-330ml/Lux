use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fmt};

use crate::{
    domain::{
        content_restrictions::{ContentRatingSystem, normalize_rating_code},
        ids::UserId,
    },
    storage::{
        Database, NewUserContentRestrictions, StorageError, UserContentRestrictions,
        UserContentRestrictionsCasResult,
    },
};

const MAX_TAGS_PER_KIND: usize = 100;
const MAX_TAG_CHARACTERS: usize = 128;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UpdateUserContentRestrictionsInput {
    pub(crate) expected_version: i64,
    pub(crate) rating_system: String,
    pub(crate) maximum_rating: Option<String>,
    pub(crate) allow_unrated: bool,
    pub(crate) allowed_tags: Vec<String>,
    pub(crate) blocked_tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserContentRestrictionPolicy {
    pub(crate) rating_system: String,
    pub(crate) maximum_rating: Option<String>,
    pub(crate) allow_unrated: bool,
    pub(crate) allowed_tags: Vec<String>,
    pub(crate) blocked_tags: Vec<String>,
    pub(crate) version: i64,
    pub(crate) updated_at: i64,
}

impl From<UserContentRestrictions> for UserContentRestrictionPolicy {
    fn from(policy: UserContentRestrictions) -> Self {
        Self {
            rating_system: policy.rating_system,
            maximum_rating: policy.maximum_rating,
            allow_unrated: policy.allow_unrated,
            allowed_tags: policy.allowed_tags,
            blocked_tags: policy.blocked_tags,
            version: policy.version,
            updated_at: policy.updated_at,
        }
    }
}

#[derive(Debug)]
pub(crate) enum UserContentRestrictionsError {
    InvalidPolicy,
    NotFound,
    VersionConflict,
    Storage(StorageError),
}

impl fmt::Display for UserContentRestrictionsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicy => formatter.write_str("content restriction policy is invalid"),
            Self::NotFound => formatter.write_str("user was not found"),
            Self::VersionConflict => formatter.write_str("content restriction version changed"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for UserContentRestrictionsError {}

impl From<StorageError> for UserContentRestrictionsError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

pub(crate) async fn get_user_content_restrictions(
    database: &Database,
    user_id: UserId,
) -> Result<Option<UserContentRestrictionPolicy>, UserContentRestrictionsError> {
    let user_id = user_id.to_string();
    if !database.user_exists(&user_id).await? {
        return Err(UserContentRestrictionsError::NotFound);
    }
    database
        .user_content_restrictions(&user_id)
        .await
        .map(|policy| policy.map(UserContentRestrictionPolicy::from))
        .map_err(Into::into)
}

pub(crate) async fn update_user_content_restrictions(
    database: &Database,
    user_id: UserId,
    input: UpdateUserContentRestrictionsInput,
) -> Result<UserContentRestrictionPolicy, UserContentRestrictionsError> {
    if input.expected_version < 0 {
        return Err(UserContentRestrictionsError::InvalidPolicy);
    }
    let rating_system = ContentRatingSystem::from_key(&input.rating_system)
        .ok_or(UserContentRestrictionsError::InvalidPolicy)?;
    let maximum_rating = input
        .maximum_rating
        .as_deref()
        .map(|rating| {
            normalize_rating_code(rating_system, rating)
                .map(str::to_owned)
                .ok_or(UserContentRestrictionsError::InvalidPolicy)
        })
        .transpose()?;
    let allowed_tags = normalize_tags(&input.allowed_tags)?;
    let blocked_tags = normalize_tags(&input.blocked_tags)?;
    let user_id = user_id.to_string();

    match database
        .compare_and_swap_user_content_restrictions(
            &user_id,
            input.expected_version,
            NewUserContentRestrictions {
                rating_system: rating_system.key(),
                maximum_rating: maximum_rating.as_deref(),
                allow_unrated: input.allow_unrated,
                allowed_tags: &allowed_tags,
                blocked_tags: &blocked_tags,
            },
        )
        .await?
    {
        UserContentRestrictionsCasResult::Updated(policy) => Ok(policy.into()),
        UserContentRestrictionsCasResult::Conflict => {
            Err(UserContentRestrictionsError::VersionConflict)
        }
        UserContentRestrictionsCasResult::UserUnavailable => {
            Err(UserContentRestrictionsError::NotFound)
        }
    }
}

fn normalize_tags(tags: &[String]) -> Result<Vec<String>, UserContentRestrictionsError> {
    if tags.len() > MAX_TAGS_PER_KIND {
        return Err(UserContentRestrictionsError::InvalidPolicy);
    }

    let mut seen = HashSet::with_capacity(tags.len());
    let mut normalized = Vec::with_capacity(tags.len());
    for tag in tags {
        if tag.chars().any(char::is_control) {
            return Err(UserContentRestrictionsError::InvalidPolicy);
        }
        let tag = tag
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        let length = tag.chars().count();
        if !(1..=MAX_TAG_CHARACTERS).contains(&length) || !seen.insert(tag.clone()) {
            return Err(UserContentRestrictionsError::InvalidPolicy);
        }
        normalized.push(tag);
    }
    normalized.sort_unstable();
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::{UserContentRestrictionsError, normalize_tags};
    use crate::domain::content_restrictions::project_content_restriction_index;

    #[test]
    fn configured_tags_use_the_same_normalization_as_the_media_index() {
        let configured = normalize_tags(&[" Action  Sci-Fi ".to_owned()]).unwrap();
        let indexed =
            project_content_restriction_index(None, &["Action Sci-Fi".to_owned()]).unwrap();

        assert_eq!(configured, indexed.tags);
    }

    #[test]
    fn configured_tags_reject_empty_long_control_and_duplicate_values() {
        let invalid_cases = [
            vec![" \t ".to_owned()],
            vec!["x".repeat(129)],
            vec!["Action\nDrama".to_owned()],
            vec!["Drama".to_owned(), " drama ".to_owned()],
        ];
        for tags in invalid_cases {
            assert!(matches!(
                normalize_tags(&tags),
                Err(UserContentRestrictionsError::InvalidPolicy)
            ));
        }

        let too_many = (0..101)
            .map(|index| format!("tag-{index}"))
            .collect::<Vec<_>>();
        assert!(matches!(
            normalize_tags(&too_many),
            Err(UserContentRestrictionsError::InvalidPolicy)
        ));
    }
}
