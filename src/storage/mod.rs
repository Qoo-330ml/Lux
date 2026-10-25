mod repository;

pub(crate) struct CatalogSearchQuery<'a> {
    pub(crate) query: &'a str,
    pub(crate) like_query: &'a str,
    pub(crate) item_types: &'a [String],
    pub(crate) library_ids: Option<&'a [String]>,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
    pub(crate) content_restrictions: Option<&'a repository::UserContentRestrictions>,
}

// LUX-564 lands the cache foundation before S3a adds its first production caller.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod sql_templates;

// LUX-684 consumes this bound and candidate contract in the background index worker.
#[allow(dead_code)]
pub(crate) const CONTENT_RESTRICTION_INDEX_BATCH_LIMIT: usize = 100;

#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MediaContentRestrictionIndexCandidate {
    pub(crate) item_id: String,
    pub(crate) nfo_metadata_json: Option<String>,
}

pub(crate) struct ContentRestrictionIndexWrite {
    pub(crate) item_id: String,
    pub(crate) expected_nfo_json: Option<String>,
    pub(crate) certification_code: Option<String>,
    pub(crate) tags: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ContentRestrictionIndexBatchOutcome {
    pub(crate) indexed: usize,
    pub(crate) stale: usize,
    pub(crate) removed: usize,
}

pub(crate) struct GlobalCollectionSourcePreference<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) is_admin: bool,
    pub(crate) collection_item_id: &'a str,
    pub(crate) work_item_id: &'a str,
    pub(crate) source_item_id: &'a str,
    pub(crate) media_source_id: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MetadataAutoMatchPolicy {
    UseLibrarySetting,
    Enabled,
    Disabled,
}

// Registration settings are consumed by LUX-803's administrator API.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum RegistrationMode {
    InvitationOnly,
    PublicEmail,
}

#[allow(dead_code)]
pub(crate) struct StoredRegistrationMode {
    pub(crate) mode: RegistrationMode,
    pub(crate) version: i64,
    raw_value: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserRegistrationStatus {
    PendingEmail,
    PendingApproval,
    Approved,
    Rejected,
    Expired,
}

impl UserRegistrationStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PendingEmail => "PENDING_EMAIL",
            Self::PendingApproval => "PENDING_APPROVAL",
            Self::Approved => "APPROVED",
            Self::Rejected => "REJECTED",
            Self::Expired => "EXPIRED",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "PENDING_EMAIL" => Ok(Self::PendingEmail),
            "PENDING_APPROVAL" => Ok(Self::PendingApproval),
            "APPROVED" => Ok(Self::Approved),
            "REJECTED" => Ok(Self::Rejected),
            "EXPIRED" => Ok(Self::Expired),
            _ => Err(StorageError::Serialization(
                "unknown user registration status".to_owned(),
            )),
        }
    }
}

pub(crate) struct NewUserRegistrationRequest<'a> {
    pub(crate) id: &'a str,
    pub(crate) recipient_email: &'a str,
    pub(crate) username_normalized: &'a str,
    pub(crate) display_name: &'a str,
    pub(crate) password_hash: &'a str,
    pub(crate) permission_snapshot_json: &'a str,
    pub(crate) expires_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserRegistrationCreateResult {
    Created,
    AlreadyExists,
    RateLimited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserRegistrationResendResult {
    Queued,
    NotResendable,
    RateLimited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserRegistrationVerifyResult {
    Verified,
    NotAcceptable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserEmailChangeVerifyResult {
    Verified,
    NotAcceptable,
    EmailConflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserEmailChangeCreateResult {
    Created,
    NotAcceptable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UserRegistrationApproveResult {
    Approved(String),
    NotReviewable,
    EmailConflict,
    UsernameConflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserRegistrationRejectResult {
    Rejected,
    NotReviewable,
}

#[allow(dead_code)] // LUX-812 maps this redacted summary to the administrator API.
pub(crate) struct StoredUserRegistrationRequest {
    pub(crate) id: String,
    pub(crate) recipient_email: String,
    pub(crate) username_normalized: String,
    pub(crate) display_name: String,
    pub(crate) status: UserRegistrationStatus,
    pub(crate) permission_snapshot_json: String,
    pub(crate) expires_at: i64,
    pub(crate) email_verified_at: Option<i64>,
    pub(crate) reviewed_at: Option<i64>,
    pub(crate) reviewed_by_user_id: Option<String>,
    pub(crate) approved_user_id: Option<String>,
    pub(crate) last_error_code: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

#[allow(dead_code)] // LUX-812 maps this bounded storage result to the administrator API.
pub(crate) struct UserRegistrationRequestPage {
    pub(crate) requests: Vec<StoredUserRegistrationRequest>,
    pub(crate) page_size: i64,
    pub(crate) total_count: i64,
}

#[allow(dead_code)] // LUX-808 resolves a submitted verification token to its challenge.
pub(crate) struct StoredUserRegistrationChallenge {
    pub(crate) id: String,
    pub(crate) expires_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UserPlaybackPolicy {
    pub(crate) can_play: bool,
    pub(crate) max_simultaneous_playbacks: i64,
    pub(crate) allow_remux: bool,
    pub(crate) allow_audio_transcode: bool,
    pub(crate) allow_video_transcode: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UserPermissionCopySnapshot {
    pub(crate) library_access_mode: String,
    pub(crate) library_ids: Vec<String>,
    pub(crate) can_download: bool,
    pub(crate) playback_policy: UserPlaybackPolicy,
    pub(crate) content_restrictions: Option<repository::UserContentRestrictions>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct UserPermissionCopyGroups {
    pub(crate) library: bool,
    pub(crate) content: bool,
    pub(crate) playback: bool,
}

impl UserPermissionCopyGroups {
    pub(crate) fn is_empty(self) -> bool {
        !self.library && !self.content && !self.playback
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserPermissionCopyResult {
    Updated,
    Conflict,
    UserUnavailable,
    InvalidSnapshot,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NewStoredUserPermissions<'a> {
    pub(crate) library_access_mode: &'a str,
    pub(crate) library_ids: &'a [String],
    pub(crate) can_download: bool,
    pub(crate) can_play: bool,
    pub(crate) max_simultaneous_playbacks: i64,
    pub(crate) allow_remux: bool,
    pub(crate) allow_audio_transcode: bool,
    pub(crate) allow_video_transcode: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredUserInvitationAcceptance {
    pub(crate) invitation_id: String,
    pub(crate) challenge_id: String,
    pub(crate) recipient_email: String,
    pub(crate) expires_at: i64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NewInvitationUserAccount<'a> {
    pub(crate) id: &'a str,
    pub(crate) username_normalized: &'a str,
    pub(crate) display_name: &'a str,
    pub(crate) password_hash: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserInvitationAcceptResult {
    Accepted,
    NotAcceptable,
    UsernameTaken,
    EmailAlreadyRegistered,
}

// LUX-645 consumes this contract in the preset application service.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ApplyUserPermissionPreset<'a> {
    pub(crate) preset_id: &'a str,
    pub(crate) preset_name: &'a str,
    pub(crate) preset_version: i64,
    pub(crate) is_admin: bool,
    pub(crate) can_manage_server: bool,
    pub(crate) permissions: NewStoredUserPermissions<'a>,
}

// LUX-620 wires this storage contract into Web, Emby, and STRM playback entry points.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlaybackAdmissionDenialReason {
    PlaybackDisabled,
    ConcurrentLimit,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlaybackAdmissionDecision {
    Admitted,
    Denied(PlaybackAdmissionDenialReason),
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PlaybackAdmissionResult {
    pub(crate) decision: PlaybackAdmissionDecision,
    pub(crate) active_playback_count: i64,
    pub(crate) max_simultaneous_playbacks: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::SelectedMetadataUpdate,
    };

    fn selection_update<'a>(
        item_id: &'a str,
        candidate_id: &'a str,
        title: &'a str,
        keep_pending: bool,
    ) -> SelectedMetadataUpdate<'a> {
        SelectedMetadataUpdate {
            item_id,
            candidate_id,
            title,
            original_title: None,
            overview: None,
            production_year: None,
            premiere_date: None,
            last_air_date: None,
            status: None,
            original_language: None,
            rating: None,
            rating_source: None,
            provider_ids_json: "{\"tmdb\":\"movie-599\"}",
            metadata_scraper_id: Some("tmdb"),
            metadata_fingerprint: &[],
            provenance_json: "{}",
            locked_fields_json: "[]",
            poster_fallback_required: false,
            keep_pending,
        }
    }

    #[test]
    fn nfo_collection_reference_requires_a_recognized_namespaced_identity() {
        let provider_ids = std::collections::BTreeMap::from([
            ("Tmdb".to_owned(), "movie-599".to_owned()),
            ("imdb".to_owned(), "tt0000599".to_owned()),
        ]);
        let tmdb = GlobalMovieCollectionReference::from_nfo(
            &provider_ids,
            Some("TMDB"),
            Some("tmdb:1281825"),
            Some("飞驰人生"),
            Some(2019),
        )
        .expect("recognized TMDb set ID");
        let scraper = GlobalMovieCollectionReference::from_scraper(
            "org.lux.scraper.tmdb",
            "movie-599",
            Some("1281825"),
            Some("飞驰人生"),
            Some(2019),
        )
        .expect("confirmed scraper collection ID");
        let unqualified = GlobalMovieCollectionReference::from_nfo(
            &provider_ids,
            Some("org.lux.scraper.tmdb"),
            Some("1281825"),
            Some("飞驰人生"),
            Some(2019),
        )
        .expect("selected scraper provides the namespace for its own NFO set ID");
        let imdb = GlobalMovieCollectionReference::from_nfo(
            &provider_ids,
            Some("TMDB"),
            Some("imdb:1281825"),
            Some("飞驰人生"),
            Some(2019),
        )
        .expect("recognized IMDb set ID");

        assert_eq!(tmdb, scraper);
        assert_eq!(tmdb, unqualified);
        assert!(
            GlobalMovieCollectionReference::from_scraper(
                "tmdb",
                "movie-599",
                Some("imdb:1281825"),
                Some("飞驰人生"),
                None,
            )
            .is_none()
        );
        assert_ne!(tmdb.collection_provider, imdb.collection_provider);
        assert_eq!(tmdb.collection_provider_id, imdb.collection_provider_id);
        assert!(
            GlobalMovieCollectionReference::from_nfo(
                &provider_ids,
                Some("TMDB"),
                Some("unknown:1281825"),
                Some("飞驰人生"),
                None,
            )
            .is_none()
        );
        assert!(
            GlobalMovieCollectionReference::from_nfo(
                &provider_ids,
                Some("TMDB"),
                Some("tmdb:bad/id"),
                Some("飞驰人生"),
                None,
            )
            .is_none()
        );
        let unnamed = GlobalMovieCollectionReference::from_nfo(
            &provider_ids,
            Some("TMDB"),
            Some("tmdb:1281825"),
            Some("  "),
            None,
        )
        .expect("reliable set ID remains usable without a collection title");
        assert_eq!(
            unnamed.collection_title,
            "__lux_pending_collection_title__:tmdb:1281825"
        );
    }

    #[tokio::test]
    async fn metadata_selection_updates_collection_reference_in_the_same_transaction()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let item_id = "collection-reference-item";
        let first_candidate_id = "collection-reference-first";
        let second_candidate_id = "collection-reference-second";
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 has_available_source
             ) VALUES (?, ?, 'MOVIE', 'Before', 'before', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        for (candidate_id, provider_id) in [(first_candidate_id, "movie-599")] {
            sqlx::query(
                "INSERT INTO metadata_candidates (
                     id, item_id, provider, provider_id, candidate_json, score, status
                 ) VALUES (?, ?, 'TMDB', ?, '{}', 100, 'PENDING')",
            )
            .bind(candidate_id)
            .bind(item_id)
            .bind(provider_id)
            .execute(database.pool())
            .await?;
        }
        let reference = GlobalMovieCollectionReference::from_scraper(
            "tmdb",
            "movie-599",
            Some("1281825"),
            Some("飞驰人生"),
            Some(2019),
        )
        .ok_or("valid confirmed collection identity")?;

        assert!(
            database
                .select_metadata_candidate_with_collection_reference(
                    selection_update(item_id, first_candidate_id, "飞驰人生", false),
                    Some(&reference),
                )
                .await?
        );
        let stored_reference: (String, String, String, String, String, Option<i64>) =
            sqlx::query_as(
                "SELECT movie_provider, movie_provider_id, collection_provider,
                        collection_provider_id, collection_title, sort_order
                 FROM collection_item_references WHERE item_id = ?",
            )
            .bind(item_id)
            .fetch_one(database.pool())
            .await?;
        assert_eq!(
            stored_reference,
            (
                "tmdb".to_owned(),
                "movie-599".to_owned(),
                "tmdb".to_owned(),
                "1281825".to_owned(),
                "飞驰人生".to_owned(),
                Some(2019),
            )
        );

        sqlx::query(
            "INSERT INTO metadata_candidates (
                 id, item_id, provider, provider_id, candidate_json, score, status
             ) VALUES (?, ?, 'TMDB', 'movie-600', '{}', 100, 'PENDING')",
        )
        .bind(second_candidate_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;

        sqlx::query(
            "CREATE TRIGGER reject_collection_reference
             BEFORE INSERT ON collection_item_references
             BEGIN SELECT RAISE(ABORT, 'reject collection reference'); END",
        )
        .execute(database.pool())
        .await?;
        assert!(
            database
                .select_metadata_candidate_with_collection_reference(
                    selection_update(item_id, second_candidate_id, "Changed", false),
                    Some(&reference),
                )
                .await
                .is_err()
        );
        let item_title: String = sqlx::query_scalar("SELECT title FROM media_items WHERE id = ?")
            .bind(item_id)
            .fetch_one(database.pool())
            .await?;
        let candidate_status: String =
            sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
                .bind(second_candidate_id)
                .fetch_one(database.pool())
                .await?;
        assert_eq!(item_title, "飞驰人生");
        assert_eq!(candidate_status, "PENDING");

        sqlx::query("DROP TRIGGER reject_collection_reference")
            .execute(database.pool())
            .await?;
        assert!(
            database
                .select_metadata_candidate_with_collection_reference(
                    selection_update(item_id, second_candidate_id, "Review", true),
                    Some(&reference),
                )
                .await?
        );
        let reference_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM collection_item_references WHERE item_id = ?")
                .bind(item_id)
                .fetch_one(database.pool())
                .await?;
        let identification_status: String =
            sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
                .bind(item_id)
                .fetch_one(database.pool())
                .await?;
        assert_eq!(reference_count, 1);
        assert_eq!(identification_status, "PENDING");

        let third_candidate_id = "collection-reference-third";
        sqlx::query(
            "INSERT INTO metadata_candidates (
                 id, item_id, provider, provider_id, candidate_json, score, status
             ) VALUES (?, ?, 'TMDB', 'movie-601', '{}', 100, 'PENDING')",
        )
        .bind(third_candidate_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
        assert!(
            database
                .select_metadata_candidate_with_collection_reference(
                    selection_update(item_id, third_candidate_id, "No collection", false),
                    None,
                )
                .await?
        );
        let missing_reference_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM collection_item_references WHERE item_id = ?")
                .bind(item_id)
                .fetch_one(database.pool())
                .await?;
        assert_eq!(missing_reference_count, 0);

        Ok(())
    }
}

impl MetadataAutoMatchPolicy {
    pub(crate) const fn allows_scraper_lookup(self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

pub(crate) struct NewItemMetadataCompletenessResult<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) capability: &'a str,
    pub(crate) input_fingerprint: &'a [u8],
    pub(crate) is_missing: bool,
    pub(crate) checked_at: i64,
}

#[derive(Clone, Copy)]
pub(crate) struct NewItemMetadataCompletenessCheck<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) capability: &'a str,
    pub(crate) input_fingerprint: &'a [u8],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MetadataFillMissingRequest {
    pub(crate) item_id: String,
    pub(crate) input_fingerprint: Option<Vec<u8>>,
    pub(crate) capabilities_json: String,
    pub(crate) automatic_retry_count: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GlobalMovieCollectionReference {
    pub(crate) movie_provider: String,
    pub(crate) movie_provider_id: String,
    pub(crate) collection_provider: String,
    pub(crate) collection_provider_id: String,
    pub(crate) collection_title: String,
    pub(crate) sort_order: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq, sqlx::FromRow)]
pub(crate) struct StoredMovieNfoCollectionReferenceReconciliation {
    pub(crate) item_id: String,
    pub(crate) provider_ids_json: Option<String>,
    pub(crate) metadata_scraper_id: Option<String>,
    pub(crate) library_scraper_id: Option<String>,
    pub(crate) production_year: Option<i64>,
    pub(crate) nfo_metadata_json: String,
}

impl GlobalMovieCollectionReference {
    pub(crate) fn identity_key(&self) -> String {
        global_movie_collection_reference_identity_key(
            &self.movie_provider,
            &self.movie_provider_id,
            &self.collection_provider,
            &self.collection_provider_id,
        )
    }

    pub(crate) fn matches_existing_nfo_identity(
        &self,
        set_id: &str,
        provider_ids_json: Option<&str>,
    ) -> bool {
        let Some(set_id) = reliable_external_id(set_id) else {
            return false;
        };
        if set_id != self.collection_provider_id {
            return false;
        }
        let Some(provider_ids) = provider_ids_json.and_then(|value| {
            serde_json::from_str::<std::collections::BTreeMap<String, String>>(value).ok()
        }) else {
            return false;
        };
        unique_provider_id(&provider_ids, &self.movie_provider)
            .is_some_and(|(_, movie_provider_id)| movie_provider_id == self.movie_provider_id)
    }

    pub(crate) fn from_scraper(
        movie_provider: &str,
        movie_provider_id: &str,
        set_id: Option<&str>,
        collection_title: Option<&str>,
        sort_order: Option<i64>,
    ) -> Option<Self> {
        let movie_provider = reliable_provider_namespace(movie_provider)?;
        let movie_provider_id = reliable_external_id(movie_provider_id)?;
        let raw_set_id = set_id?.trim();
        let collection_provider_id =
            if let Some((provider, collection_id)) = raw_set_id.split_once(':') {
                if reliable_provider_namespace(provider)? != movie_provider {
                    return None;
                }
                reliable_external_id(collection_id)?
            } else {
                reliable_external_id(raw_set_id)?
            };
        let collection_title = collection_title
            .and_then(reliable_collection_title)
            .unwrap_or_else(|| {
                collection_title_placeholder(&movie_provider, &collection_provider_id)
            });
        Some(Self {
            movie_provider: movie_provider.clone(),
            movie_provider_id,
            collection_provider: movie_provider,
            collection_provider_id,
            collection_title,
            sort_order: sort_order.filter(|value| *value > 0),
        })
    }

    pub(crate) fn from_nfo(
        movie_provider_ids: &std::collections::BTreeMap<String, String>,
        selected_movie_provider: Option<&str>,
        set_id: Option<&str>,
        collection_title: Option<&str>,
        sort_order: Option<i64>,
    ) -> Option<Self> {
        let raw_set_id = set_id?.trim();
        let (collection_provider, collection_provider_id) =
            if let Some((provider, provider_id)) = raw_set_id.split_once(':') {
                (
                    reliable_provider_namespace(provider)?,
                    reliable_external_id(provider_id)?,
                )
            } else {
                (
                    reliable_provider_namespace(selected_movie_provider?)?,
                    reliable_external_id(raw_set_id)?,
                )
            };
        let namespace_is_recognized = unique_provider_id(movie_provider_ids, &collection_provider)
            .is_some()
            || selected_movie_provider
                .and_then(reliable_provider_namespace)
                .is_some_and(|provider| provider == collection_provider);
        if !namespace_is_recognized {
            return None;
        }
        let collection_title = collection_title
            .and_then(reliable_collection_title)
            .unwrap_or_else(|| {
                collection_title_placeholder(&collection_provider, &collection_provider_id)
            });
        let movie_identity =
            unique_provider_id(movie_provider_ids, &collection_provider).or_else(|| {
                selected_movie_provider.and_then(|provider| {
                    let provider = reliable_provider_namespace(provider)?;
                    unique_provider_id(movie_provider_ids, &provider)
                })
            })?;
        Some(Self {
            movie_provider: movie_identity.0,
            movie_provider_id: movie_identity.1,
            collection_provider,
            collection_provider_id,
            collection_title,
            sort_order: sort_order.filter(|value| *value > 0),
        })
    }
}

pub(crate) struct GlobalMovieCollectionReferenceUpdate<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) reference: Option<&'a GlobalMovieCollectionReference>,
    pub(crate) unqualified_nfo_set_id: Option<&'a str>,
    pub(crate) nfo_collection_reference_key: &'a str,
}

pub(crate) fn global_movie_collection_reference_identity_key(
    movie_provider: &str,
    movie_provider_id: &str,
    collection_provider: &str,
    collection_provider_id: &str,
) -> String {
    format!("{movie_provider}:{movie_provider_id}:{collection_provider}:{collection_provider_id}")
}

const PENDING_GLOBAL_COLLECTION_TITLE_PREFIX: &str = "__lux_pending_collection_title__:";

pub(crate) fn global_collection_title_is_placeholder(title: &str) -> bool {
    title.starts_with(PENDING_GLOBAL_COLLECTION_TITLE_PREFIX)
}

pub(crate) fn global_collection_title_fallback(provider: &str, collection_id: &str) -> String {
    format!("合集 {provider}:{collection_id}")
}

fn reliable_provider_namespace(value: &str) -> Option<String> {
    let provider = value
        .trim()
        .rsplit(['.', ':', '/'])
        .next()?
        .trim()
        .to_ascii_lowercase();
    (!provider.is_empty()
        && provider.len() <= 128
        && provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    .then_some(provider)
}

fn reliable_external_id(value: &str) -> Option<String> {
    let id = value.trim();
    (!id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    .then(|| id.to_owned())
}

fn reliable_collection_title(value: &str) -> Option<String> {
    let title = value.trim();
    (!title.is_empty() && title.len() <= 1024).then(|| title.to_owned())
}

fn collection_title_placeholder(provider: &str, collection_id: &str) -> String {
    format!("{PENDING_GLOBAL_COLLECTION_TITLE_PREFIX}{provider}:{collection_id}")
}

fn unique_provider_id(
    provider_ids: &std::collections::BTreeMap<String, String>,
    provider: &str,
) -> Option<(String, String)> {
    let mut matches = provider_ids.iter().filter_map(|(key, value)| {
        let key = reliable_provider_namespace(key)?;
        let value = reliable_external_id(value)?;
        key.eq_ignore_ascii_case(provider).then_some((key, value))
    });
    let matched = matches.next()?;
    matches
        .all(|candidate| candidate == matched)
        .then_some(matched)
}

#[derive(Debug, Default)]
pub(crate) struct ItemMetadataCompletenessCommit {
    pub(crate) updated_count: usize,
    pub(crate) scheduled_job_ids: Vec<String>,
}

pub(crate) use repository::GLOBAL_COLLECTION_LIBRARY_ID;
pub(crate) use repository::MAX_PLAYBACK_SESSION_WINDOW_SECONDS;
pub use repository::{
    Database, DatabaseDiagnosticsSnapshot, DatabaseLifecycleCleanupReport, PersonListOptions,
    PersonSort, StorageError,
};

#[allow(unused_imports)]
pub(crate) use repository::{
    AdminUserSortField, AdminUserStatusFilter, CatalogFilterQuery, CatalogSort,
    ChapterDetectionOutcomeUpdate, ChapterDetectionSourceStateUpdate,
    DEFAULT_MAINTENANCE_INITIAL_DELAY_SECONDS, DEFAULT_MAINTENANCE_INTERVAL_SECONDS,
    DEFAULT_PLAYED_PERCENT, DashboardStats, DatabasePoolSnapshot, DevicePairingRedeemResult,
    EmbyMigrationHandledItemBatch, EmbyMigrationImportRecordBatch, EmbyMigrationItemMatchBatch,
    EmbyMigrationItemPageBatch, EmbyMigrationJobProgress, EmbyMigrationPersonFavoriteBatch,
    EmbyMigrationPersonFavoriteStateBatch, EmbyMigrationUserItemStateBatch,
    EmbyMigrationUserItemStateFields, ExternalSubtitleUpdate, FilesystemEntryMove,
    FilesystemEntryUpdate, ItemImageBatchInsert, ItemImageInsert, ItemImageMetadata,
    LibraryDeletionBatch, LibrarySettingsUpdate, LocalNfoDefaultsRepair,
    MANIFEST_POSTPROCESSING_TARGET_PAGE_SIZE, MailChallengeKind, MailDeliveryErrorCode,
    MailOutboxPage, MailOutboxStatus, MailOutboxTemplateKind, MaintenanceCandidate,
    MaintenanceReport, ManifestDeltaBatchCommit, ManifestDeltaBatchCommitResult,
    ManifestDiscoveryCommitResult, ManifestExistingFileUpdate,
    ManifestPostprocessingTargetBatchResult, ManifestPostprocessingTargetPage,
    MediaInfoChapterUpdate, MediaMergeOrigin, MediaMetadataUpdate, MediaProbeUpdate,
    MediaStreamUpdate, MetadataCapabilityResult, MetadataImageAttemptUpdate,
    MetadataImageUnavailable, MigrationMediaIdentityLookup, MigrationPersonIdentityLookup,
    NewAccessToken, NewAuditEvent, NewChapterDetectionJob, NewChapterDetectionJobItem,
    NewCollection, NewDanmakuMatchJob, NewDanmakuTrack, NewDeviceAccessToken, NewDevicePairing,
    NewEmbyDirectPlaybackTicket, NewEmbyMigrationJob, NewEpisodeFile, NewFilesystemEntry,
    NewHierarchyItem, NewLibrary, NewLibraryRoot, NewMailChallenge, NewMailOutboxMessage,
    NewMediaChapterMarker, NewMediaItem, NewMediaSource, NewMetadataCandidate, NewMovieFile,
    NewNotificationDestination, NewNotificationEvent, NewPersonCredit, NewPlaybackEvent,
    NewScanLocalMetadataBatch, NewScanManifest, NewScanManifestDelta,
    NewScanManifestDiscoveryChunk, NewScanManifestEntry, NewScanManifestFilesystemEntry,
    NewScanManifestIndexedFile, NewScanManifestPositiveIndex, NewScanManifestRoot,
    NewScanManifestSeenFilesystemEntry, NewScanManifestSidecarEntry, NewScanManifestUnresolvedFile,
    NewStoredUser, NewStrmProbeJob, NewTranscodingHistory, NewUserAccessBlacklistRule,
    NewUserAccountPreferences, NewUserActivityEvent, NewUserContentRestrictions,
    NewUserDeviceObservation, NewUserInvitation, NewWebPlaybackEvent, NewWebPlaybackSession,
    PLAYBACK_SESSION_STALE_AFTER_SECONDS, PersonMatchCandidateRestore, ReconciliationBatchCommit,
    ReconciliationBatchCommitResult, ResumeItemsQuery, ScanFingerprintMigrationCommit,
    ScanManifestRemovalCandidateQuery, SelectedMetadataUpdate, StoredAccessTokenDevice,
    StoredActivityEvent, StoredAdminUserListItem, StoredCanonicalPerson,
    StoredCanonicalPersonMatch, StoredCatalogDetail, StoredCatalogImageTag,
    StoredCatalogItemCounts, StoredCatalogRow, StoredChapterDetectionItem,
    StoredChapterDetectionJob, StoredChapterDetectionSource, StoredChapterDetectionSourceState,
    StoredCollectionRefresh, StoredDanmakuMatchItem, StoredDanmakuMatchJob, StoredDanmakuSource,
    StoredDownloadSource, StoredEmbyCollection, StoredEmbyDirectPlaybackTicket,
    StoredEmbyLoginSessionSummary, StoredEmbyMigrationImportRecord, StoredEmbyMigrationItemMatch,
    StoredEmbyMigrationJob, StoredEmbyMigrationPersonFavorite, StoredEmbyMigrationSource,
    StoredEmbyMigrationUserBinding, StoredEmbyMigrationUserLink, StoredEpisodeIdentityCandidate,
    StoredExternalSubtitle, StoredFilesystemEntry, StoredFolderScanPath, StoredGlobalCollection,
    StoredGlobalCollectionSource, StoredIdentityMergeCandidateItem,
    StoredIdentityMergeCandidateKey, StoredImageIdentity, StoredItemImage,
    StoredItemImageCandidate, StoredItemImagePathConflict, StoredItemMetadataCompleteness,
    StoredItemScanPath, StoredItemSourceLocator, StoredJobActivityItem, StoredLibrary,
    StoredLibraryCoverJob, StoredLibraryDeletionJob, StoredLibraryIdentity,
    StoredLibraryPipelineActivity, StoredLibraryPipelineActivityPage, StoredLibraryPoster,
    StoredLibraryRoot, StoredLibraryScraper, StoredMailChallenge, StoredMailOutboxMessage,
    StoredMediaChapter, StoredMediaItem, StoredMediaItemKind, StoredMediaMerge,
    StoredMediaMergeRecord, StoredMediaMergeUndo, StoredMediaMetadata, StoredMediaSourcePath,
    StoredMediaWritebackContext, StoredMetadataCandidate, StoredMetadataCapabilityAttempt,
    StoredMetadataImageAttempt, StoredMetadataReidentifyItem, StoredMetadataReidentifyJob,
    StoredMigrationMediaIdentity, StoredMigrationPersonIdentity, StoredMovieIdentity,
    StoredNotificationDelivery, StoredNotificationDestination, StoredPersonCredit,
    StoredPersonIdentityMove, StoredPersonIndexRebuildJob, StoredPersonMatchCandidate,
    StoredPipelineStageActivity, StoredPipelineStageKind, StoredPipelineStatusCounts,
    StoredPlaybackHistoryEvent, StoredPlaybackSession, StoredPlaybackSource,
    StoredReconciliationScanEntry, StoredScanFingerprintMigrationCandidate,
    StoredScanFingerprintMigrationCursor, StoredScanJob, StoredScanJobCounts,
    StoredScanJobMetadataPage, StoredScanJobMetadataSources, StoredScanJobPath,
    StoredScanJobRetryState, StoredScanLocalMetadataBackfillPage, StoredScanLocalMetadataBatch,
    StoredScanLocalMetadataSource, StoredScanManifest, StoredScanManifestDelta,
    StoredScanManifestDiffCandidate, StoredScanManifestDirectory,
    StoredScanManifestFilesystemBaseline, StoredScanManifestPostprocessingRoot,
    StoredScanManifestPostprocessingState, StoredScanManifestRemovalCandidate,
    StoredScheduledTaskConfig, StoredScheduledTaskPlan, StoredScheduledTaskPlanLibrary,
    StoredSeriesMetadataSource, StoredStrmMediaSource, StoredStrmProbeJob, StoredSubtitleStream,
    StoredThumbnailScraperRetry, StoredThumbnailSource, StoredTranscodingHistory,
    StoredTranscodingSessionRow, StoredUser, StoredUserAccessBlacklistRule,
    StoredUserActivityEvent, StoredUserDevice, StoredUserInvitation, StoredUserItemState,
    StoredUserPermissionPresetSource, StoredWebPlaybackAccessContext, StoredWebPlaybackSession,
    StoredWebSession, StoredWebSessionSummary, UpdateNotificationDestination, UpdateUser,
    UserAccessBlacklistIpRuleQuery, UserAccessBlacklistRuleMatchQuery,
    UserAccessBlacklistRulePageQuery, UserAccessBlacklistRuleScope, UserAccessSnapshot,
    UserAccountPreferences, UserAccountPreferencesCasResult, UserActivityEventQuery,
    UserContentRestrictions, UserContentRestrictionsCasResult, UserDevicePageQuery,
    UserInvitationCreateResult, UserInvitationPage, UserInvitationResendResult,
    UserInvitationStatus, WebPlaybackEventClaim, WebPlaybackTranscodingDetails,
    is_lite_manifest_discovery, movie_parent_folder_identity, recommendation_batch_key_at,
};

/// Opens a throwaway SQLite database for unit tests.
#[cfg(test)]
pub(crate) async fn test_database()
-> Result<(tempfile::TempDir, Database), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&crate::config::Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    Ok((temp_dir, database))
}
