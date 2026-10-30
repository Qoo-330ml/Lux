use std::{fmt, sync::Arc, time::Instant};

use crate::{
    auth::cache::ExpiringCache,
    domain::{
        content_restrictions::{
            ContentRestrictionClassification, ContentRestrictionPolicy,
            content_classification_is_allowed, content_restriction_policy_is_restrictive,
        },
        ids::UserId,
    },
    storage::{
        Database, GLOBAL_COLLECTION_LIBRARY_ID, StorageError, StoredPlaybackSource,
        UserContentRestrictions,
    },
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

/// The per-user parental-control policy, resolved once per cache window.
///
/// Only the policy is cached. Library access is still checked per request: the access layer must
/// never answer from a value that a database write outside the storage API could have invalidated,
/// and library access rows are exactly such an input (the frozen end-to-end tests change them with
/// direct SQL). Per-item certification rows are likewise read per item, so a rescan is visible
/// immediately.
#[derive(Clone)]
struct CachedRestrictionPolicy {
    restrictions: Option<UserContentRestrictions>,
}

#[derive(Clone)]
pub struct MediaAccessService {
    database: Database,
    restriction_cache: Arc<ExpiringCache<CachedRestrictionPolicy>>,
}

impl MediaAccessService {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            restriction_cache: Arc::new(ExpiringCache::default()),
        }
    }

    /// Reads the user's parental-control policy, reusing it while its epochs are unchanged.
    async fn restriction_policy(
        &self,
        user_id: UserId,
    ) -> Result<Arc<CachedRestrictionPolicy>, AccessError> {
        let user_id = user_id.to_string();
        let epochs = self.database.auth_cache();
        if let Some(entry) = self
            .restriction_cache
            .lookup(user_id.as_bytes(), Instant::now())
            && epochs.access_is_current(entry.snapshot(), entry.user_id())
        {
            return Ok(entry.value());
        }
        // Fence the read: a policy write that commits while this query runs must not leave a
        // stale value behind.
        let fence = epochs.write_fence();
        let restrictions = self.database.user_content_restrictions(&user_id).await?;
        let policy = CachedRestrictionPolicy { restrictions };
        if epochs.fence_is_current(fence) {
            let snapshot = epochs.snapshot(&user_id);
            self.restriction_cache.store(
                user_id.clone().into_bytes(),
                user_id,
                snapshot,
                policy.clone(),
                Instant::now(),
            );
        }
        Ok(Arc::new(policy))
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
        let library_id = self.database.find_item_library_id(item_id).await?;
        self.can_view_item_in_library(principal, item_id, library_id.as_deref())
            .await
    }

    /// Same check as [`Self::can_view_item`] for callers that already resolved the item library.
    ///
    /// `library_id` is this request's `Database::find_item_library_id` result for `item_id`, so
    /// `None` means the item is not visible and access is denied without repeating the lookup.
    /// Playback callbacks need the library id anyway, and without this entry point they resolve
    /// the same library twice per event.
    pub async fn can_view_item_in_library(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
        library_id: Option<&str>,
    ) -> Result<bool, AccessError> {
        let Some(library_id) = library_id else {
            return Ok(false);
        };
        if library_id == GLOBAL_COLLECTION_LIBRARY_ID {
            return Ok(false);
        }
        if principal.is_admin {
            return Ok(true);
        }
        if !self.can_view_library(principal, library_id).await? {
            return Ok(false);
        }
        self.can_view_item_content(principal, item_id).await
    }

    /// Checks only the per-user content restrictions. The caller has already confirmed that the
    /// item exists and that the principal may view its library.
    async fn can_view_item_content(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
    ) -> Result<bool, AccessError> {
        if principal.is_admin {
            return Ok(true);
        }
        let Some(user_id) = principal.user_id else {
            return Ok(true);
        };
        let policy = self.restriction_policy(user_id).await?;
        let Some(restrictions) = policy.restrictions.as_ref() else {
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

    /// Resolves accessible library ids for several users with one bounded batch.
    ///
    /// Administrators see every enabled media library, exactly like [`Self::accessible_library_ids`];
    /// the remaining users are resolved in one batched query.
    pub async fn accessible_library_ids_for_users(
        &self,
        users: &[(String, bool)],
    ) -> Result<std::collections::HashMap<String, Vec<String>>, AccessError> {
        if users.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let admin_ids = users
            .iter()
            .filter(|(_, is_admin)| *is_admin)
            .map(|(user_id, _)| user_id.clone())
            .collect::<Vec<_>>();
        let member_ids = users
            .iter()
            .filter(|(_, is_admin)| !*is_admin)
            .map(|(user_id, _)| user_id.clone())
            .collect::<Vec<_>>();
        let mut accessible = std::collections::HashMap::with_capacity(users.len());
        if !admin_ids.is_empty() {
            let enabled_library_ids = self.database.list_enabled_media_library_ids().await?;
            for user_id in admin_ids {
                accessible.insert(user_id, enabled_library_ids.clone());
            }
        }
        if !member_ids.is_empty() {
            let member_accessible = self
                .database
                .list_accessible_library_ids_for_users(&member_ids)
                .await?;
            for user_id in member_ids {
                accessible.insert(
                    user_id.clone(),
                    member_accessible.get(&user_id).cloned().unwrap_or_default(),
                );
            }
        }
        Ok(accessible)
    }

    pub(crate) async fn authorized_playback_source(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<StoredPlaybackSource>, AccessError> {
        let Some(source) = self
            .database
            .find_authorized_playback_source(
                item_id,
                source_id,
                &principal.user_id_string(),
                principal.is_admin,
            )
            .await?
        else {
            return Ok(None);
        };
        // Streaming and downloads reuse this lookup; without the content check they would
        // bypass the parental-control policy that item detail and subtitles enforce.
        if !self.can_view_item_content(principal, item_id).await? {
            return Ok(None);
        }
        Ok(Some(source))
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
    async fn batched_accessible_library_ids_match_single_principals()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let users = UserStore::new(database.clone())?;
        let admin = users
            .create_user("batch-scope-admin", "Admin", "test password", true)
            .await?;
        let all_user = users
            .create_user("batch-scope-all", "All", "test password", false)
            .await?;
        let selected_user = users
            .create_user("batch-scope-selected", "Selected", "test password", false)
            .await?;
        let libraries = LibraryService::new(database.clone());
        libraries
            .create_library("Batch scope A", LibraryKind::Movie, false)
            .await?;
        let library_b = libraries
            .create_library("Batch scope B", LibraryKind::Movie, false)
            .await?;
        database
            .set_user_library_access(
                &selected_user.id.to_string(),
                &library_b.id.to_string(),
                true,
            )
            .await?;
        let service = MediaAccessService::new(database.clone());
        let requests = vec![
            (admin.id.to_string(), true),
            (all_user.id.to_string(), false),
            (selected_user.id.to_string(), false),
        ];
        let batched = service.accessible_library_ids_for_users(&requests).await?;
        for (user_id, is_admin) in &requests {
            let single = service
                .accessible_library_ids(AccessPrincipal::new(
                    (*user_id).parse::<crate::domain::ids::UserId>()?,
                    *is_admin,
                ))
                .await?;
            assert_eq!(
                batched.get(user_id).cloned().unwrap_or_default(),
                single,
                "batched accessible libraries must match the single lookup for {user_id}"
            );
        }
        assert_eq!(
            batched.get(&admin.id.to_string()).map(Vec::len),
            Some(2),
            "administrators see every enabled media library"
        );
        assert_eq!(
            batched.get(&selected_user.id.to_string()).map(Vec::len),
            Some(1),
            "selected users see only their granted libraries"
        );
        database.close().await;
        Ok(())
    }

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

        database.reset_query_count();
        assert!(access.can_view_item(principal, "policy-episode").await?);
        assert_eq!(
            database.query_count(),
            2,
            "a warm policy cache re-reads only the item library and the library access, not the policy"
        );

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

    #[tokio::test]
    async fn authorized_playback_source_respects_content_restrictions()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let user = UserStore::new(database.clone())?
            .create_user("playback-policy-viewer", "Viewer", "test password", false)
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Playback policy", LibraryKind::Movie, false)
            .await?;
        let user_id = user.id.to_string();
        let library_id = library.id.to_string();
        database
            .set_user_library_access(&user_id, &library_id, true)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, has_available_source
             ) VALUES ('policy-movie', ?, 'MOVIE', 'Policy', 'policy', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_content_restriction_index (item_id, certification_code)
             VALUES ('policy-movie', 'R')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('policy-root', ?, '/media/policy', '/media/policy', 1, 1)",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation, is_missing
             ) VALUES ('policy-entry', 'policy-root', 'Policy.mkv', 'FILE', 1, 1, 'gen', 0)",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_sources (id, item_id, source_kind, filesystem_entry_id, is_default)
             VALUES ('policy-source', 'policy-movie', 'LOCAL_FILE', 'policy-entry', 1)",
        )
        .execute(database.pool())
        .await?;

        let access = MediaAccessService::new(database.clone());
        let principal = AccessPrincipal::new(user.id, false);
        assert!(access.can_view_item(principal, "policy-movie").await?);
        assert!(
            access
                .authorized_playback_source(principal, "policy-movie", None)
                .await?
                .is_some()
        );

        let restricted = database
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
            restricted,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        assert!(!access.can_view_item(principal, "policy-movie").await?);
        assert!(
            access
                .authorized_playback_source(principal, "policy-movie", None)
                .await?
                .is_none(),
            "playback authorization must not bypass content restrictions"
        );
        Ok(())
    }

    async fn library_fixture() -> Result<
        (
            Database,
            MediaAccessService,
            AccessPrincipal,
            String,
            (String, String),
        ),
        Box<dyn std::error::Error>,
    > {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        Box::leak(Box::new(directory));
        let user = UserStore::new(database.clone())?
            .create_user("access-cache-viewer", "Viewer", "test password", false)
            .await?;
        let libraries = LibraryService::new(database.clone());
        let first = libraries
            .create_library("Access cache A", LibraryKind::Movie, false)
            .await?;
        let second = libraries
            .create_library("Access cache B", LibraryKind::Movie, false)
            .await?;
        let service = MediaAccessService::new(database.clone());
        Ok((
            database,
            service,
            AccessPrincipal::new(user.id, false),
            user.id.to_string(),
            (first.id.to_string(), second.id.to_string()),
        ))
    }

    #[tokio::test]
    async fn library_access_changes_apply_to_the_very_next_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, access, principal, user_id, (first, second)) = library_fixture().await?;

        // A fresh user sees every enabled library; a selection then narrows the set.
        assert_eq!(access.accessible_library_ids(principal).await?.len(), 2);
        database
            .set_user_library_access(&user_id, &first, true)
            .await?;
        assert_eq!(
            access.accessible_library_ids(principal).await?,
            vec![first.clone()]
        );

        database
            .set_user_library_access(&user_id, &second, true)
            .await?;
        let refreshed = access.accessible_library_ids(principal).await?;
        assert!(
            refreshed.contains(&second),
            "granting a library must be visible to the very next request"
        );

        // Revoking it again is equally immediate.
        database
            .set_user_library_access(&user_id, &second, false)
            .await?;
        assert!(
            !access
                .accessible_library_ids(principal)
                .await?
                .contains(&second)
        );
        Ok(())
    }

    #[tokio::test]
    async fn library_access_changes_deny_the_very_next_item_check()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, access, principal, user_id, (first, second)) = library_fixture().await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, has_available_source
             ) VALUES ('access-cache-movie', ?, 'MOVIE', 'Cached', 'cached', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(&first)
        .execute(database.pool())
        .await?;
        // A selection keeps the user in SELECTED mode, so revoking one library below narrows the
        // set instead of falling back to "all enabled libraries".
        database
            .set_user_library_access(&user_id, &second, true)
            .await?;
        database
            .set_user_library_access(&user_id, &first, true)
            .await?;
        assert!(
            access
                .can_view_item(principal, "access-cache-movie")
                .await?
        );

        // Revoking the library must deny the item on the very next check, cache or not.
        database
            .set_user_library_access(&user_id, &first, false)
            .await?;
        assert!(
            !access
                .can_view_item(principal, "access-cache-movie")
                .await?
        );
        Ok(())
    }

    #[tokio::test]
    async fn library_disable_denies_the_very_next_request() -> Result<(), Box<dyn std::error::Error>>
    {
        use crate::storage::LibrarySettingsUpdate;

        let (database, access, principal, _user_id, (first, _second)) = library_fixture().await?;
        assert!(access.can_view_library(principal, &first).await?);

        database
            .update_library_settings(
                &first,
                LibrarySettingsUpdate {
                    name: None,
                    kind: None,
                    is_enabled: Some(false),
                    realtime_watch_enabled: None,
                    realtime_metadata_auto_match_enabled: None,
                    reconciliation_schedule: None,
                    metadata_schedule: None,
                    scan_concurrency: None,
                    probe_concurrency: None,
                    scraper_id: None,
                    scrapers: None,
                    chapter_source_id: None,
                    media_strategy_json: None,
                },
            )
            .await?;
        assert!(
            !access.can_view_library(principal, &first).await?,
            "disabling a library must deny access on the very next request"
        );
        Ok(())
    }

    #[tokio::test]
    async fn resolved_library_access_check_skips_the_item_lookup()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let user = UserStore::new(database.clone())?
            .create_user("resolved-library-viewer", "Viewer", "test password", false)
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Resolved library", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        database
            .set_user_library_access(&user.id.to_string(), &library_id, true)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, has_available_source
             ) VALUES ('resolved-movie', ?, 'MOVIE', 'Resolved', 'resolved', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await?;

        let access = MediaAccessService::new(database.clone());
        let principal = AccessPrincipal::new(user.id, false);
        // The first call fills the access context; the counts below measure the steady state.
        assert!(
            access
                .can_view_item_in_library(principal, "resolved-movie", Some(&library_id))
                .await?
        );
        database.reset_query_count();
        assert!(
            access
                .can_view_item_in_library(principal, "resolved-movie", Some(&library_id))
                .await?
        );
        assert_eq!(
            database.query_count(),
            1,
            "a warm policy cache checks library access but does not re-read the policy"
        );

        database.reset_query_count();
        assert!(access.can_view_item(principal, "resolved-movie").await?);
        assert_eq!(
            database.query_count(),
            2,
            "can_view_item re-resolves the item library; the resolved variant must not"
        );

        database.reset_query_count();
        assert!(
            !access
                .can_view_item_in_library(principal, "resolved-movie", None)
                .await?,
            "an unresolved library denies access exactly like a hidden item"
        );
        assert_eq!(
            database.query_count(),
            0,
            "denying an unresolved library must not fall back to the item lookup"
        );
        Ok(())
    }
}
