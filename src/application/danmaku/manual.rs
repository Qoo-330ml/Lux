//! Administrator-facing manual match (ADR-052 P5): candidate search, the single-item job behind
//! `POST /api/v1/admin/items/{itemId}/danmaku/match`, and sidecar removal.
//!
//! Everything that reaches the plugin or the filesystem is validated here instead of in the HTTP
//! handlers: the search query and page bound, the selected provider/anime/episode identifiers, the
//! media-root check before a file is deleted, and the provider rule that protects a user's own
//! sidecar from an accidental delete.

use std::path::Path;

use tokio::fs;
use uuid::Uuid;

use super::{
    DanmakuService, DanmakuServiceError,
    path::{DanmakuSourcePathError, safe_danmaku_source_paths},
};
use crate::{
    application::{
        plugin_protocol::{
            DanmakuSearchCandidate, MAX_DANMAKU_IDENTIFIER_CHARS, MAX_DANMAKU_PROVIDER_CHARS,
            MAX_DANMAKU_SEARCH_LIMIT, MAX_DANMAKU_SEARCH_QUERY_CHARS,
        },
        plugins::PluginServiceError,
    },
    storage::{NewDanmakuMatchJob, NewDanmakuSingleItemJob, StoredDanmakuSource},
};

/// Candidate page the admin API uses when the request omits `limit`.
pub const DEFAULT_MANUAL_SEARCH_LIMIT: u32 = 20;
/// Item types an administrator may match danmaku for; a series or season container has no sidecar.
const MATCHABLE_ITEM_TYPES: [&str; 2] = ["MOVIE", "EPISODE"];

/// The queued manual-match job plus the media source the request actually resolved.
///
/// The resolved source id is what the audit event records: the request may omit `sourceId`.
pub struct ManualMatchJobStart {
    pub job: super::DanmakuMatchJob,
    pub source_id: String,
}

/// One administrator-selected episode to fetch danmaku for.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ManualMatchTarget {
    /// Media source of the item; the item's default source when omitted.
    pub source_id: Option<String>,
    /// Opaque provider label the search result carried; the plugin may not need one.
    pub provider: Option<String>,
    /// Provider-side series identifier, kept on the track row for later repairs.
    pub anime_id: Option<String>,
    /// Provider-side episode identifier the worker fetches.
    pub episode_id: String,
    /// Replace an existing valid sidecar instead of recording the item as skipped.
    pub overwrite: bool,
}

impl ManualMatchTarget {
    /// Trims every identifier and applies the plugin-contract length and character rules.
    ///
    /// A blank `provider`/`animeId` means "not set", exactly like an omitted field: the plugin
    /// contract treats an empty provider as unset, and an empty anime id carries no information.
    pub fn validated(self) -> Result<Self, DanmakuServiceError> {
        let episode_id =
            normalized_manual_identifier(&self.episode_id, MAX_DANMAKU_IDENTIFIER_CHARS)
                .ok_or(DanmakuServiceError::InvalidManualTarget)?;
        let provider = optional_manual_identifier(self.provider, MAX_DANMAKU_PROVIDER_CHARS)?;
        let anime_id = optional_manual_identifier(self.anime_id, MAX_DANMAKU_IDENTIFIER_CHARS)?;
        Ok(Self {
            source_id: self.source_id,
            provider,
            anime_id,
            episode_id,
            overwrite: self.overwrite,
        })
    }
}

impl DanmakuService {
    /// Searches candidates for one item through the plugin (`danmaku.search`).
    ///
    /// The query defaults to the item's own title (the series title for an episode), because the
    /// administrator usually knows the library entry rather than the provider's spelling. Only the
    /// trimmed query and the bounded page reach the plugin; no path, URL or user data is sent.
    pub async fn search_for_item(
        &self,
        item_id: &str,
        query: Option<&str>,
        source_id: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Vec<DanmakuSearchCandidate>, DanmakuServiceError> {
        let limit = limit.unwrap_or(DEFAULT_MANUAL_SEARCH_LIMIT);
        if !(1..=MAX_DANMAKU_SEARCH_LIMIT).contains(&limit) {
            return Err(DanmakuServiceError::InvalidManualSearch);
        }
        let source = self.matchable_source(item_id, source_id).await?;
        let query = match query.map(str::trim).filter(|value| !value.is_empty()) {
            Some(query) => query.to_owned(),
            None => {
                default_search_query(&source).ok_or(DanmakuServiceError::InvalidManualSearch)?
            }
        };
        if query.chars().count() > MAX_DANMAKU_SEARCH_QUERY_CHARS
            || query.chars().any(char::is_control)
        {
            return Err(DanmakuServiceError::InvalidManualSearch);
        }
        let plugins = self
            .plugins
            .as_ref()
            .ok_or(DanmakuServiceError::ManualMatchUnavailable)?;
        // No separate "is manual match supported" probe: the plugin call already distinguishes a
        // plugin that never declared `danmaku.search` (unsupported) from one that is disabled or
        // not configured yet (unavailable), and the two must not be reported as the same thing.
        let result = plugins
            .search_danmaku(&query, limit)
            .await
            .map_err(manual_match_plugin_error)?;
        Ok(result.candidates)
    }

    /// Creates the single-item background job of an administrator's manual match.
    ///
    /// The job reuses `danmaku_match_jobs`/`danmaku_match_job_items` with `total_count = 1` and the
    /// item's library, so cancellation, listing and the retention sweep keep working. The selected
    /// episode and the provider label the administrator chose are stored on the job item, and the
    /// job is marked `MANUAL`; `danmaku_tracks` is not touched until the worker has written the
    /// file, so a user's own sidecar can never be relabelled as Lux-written by a queued job.
    ///
    /// Unlike an automatic library sweep, a manual match does not require the library to be selected
    /// in the danmaku plugin configuration: it is an explicit administrator action on one item
    /// (`docs/LUX-DEVELOPMENT.md` §3.7, ADR-052 P5). The worker applies the same exception.
    pub async fn create_manual_job(
        &self,
        item_id: &str,
        target: ManualMatchTarget,
    ) -> Result<ManualMatchJobStart, DanmakuServiceError> {
        let target = target.validated()?;
        let source = self
            .matchable_source(item_id, target.source_id.as_deref())
            .await?;
        let library_id = self
            .database
            .find_item_library_id(item_id)
            .await?
            .ok_or(DanmakuServiceError::SourceNotFound)?;
        let plugins = self
            .plugins
            .as_ref()
            .ok_or(DanmakuServiceError::ManualMatchUnavailable)?;
        plugins
            .require_danmaku_manual_match_plugin()
            .await
            .map_err(manual_match_plugin_error)?;
        let settings = plugins
            .danmaku_settings()
            .await
            .map_err(|_| DanmakuServiceError::ManualMatchUnavailable)?;
        // A manual match names one item explicitly, so it does not need the library to be selected
        // for automatic sweeps (`docs/LUX-DEVELOPMENT.md` §3.7, ADR-052 P5).
        if self
            .database
            .has_active_danmaku_match_jobs(&library_id)
            .await?
        {
            return Err(DanmakuServiceError::AlreadyActive);
        }
        let job_id = Uuid::now_v7().to_string();
        let item_row_id = format!("{job_id}:{}", source.source_id);
        let concurrency = settings.concurrency.clamp(0, super::job::MAX_CONCURRENCY);
        self.database
            .create_danmaku_single_item_job(
                NewDanmakuMatchJob {
                    id: &job_id,
                    library_id: &library_id,
                    overwrite: target.overwrite,
                    concurrency,
                },
                NewDanmakuSingleItemJob {
                    id: &item_row_id,
                    media_source_id: &source.source_id,
                    manual: true,
                    provider_anime_id: target.anime_id.as_deref(),
                    provider_episode_id: Some(&target.episode_id),
                    provider: target.provider.as_deref(),
                },
            )
            .await?;
        Ok(ManualMatchJobStart {
            job: self.get(&job_id).await?,
            source_id: source.source_id,
        })
    }

    /// Removes the registered sidecar file and its `danmaku_tracks` row of one media source.
    ///
    /// A track without a provider is a sidecar the user put next to the media file, so it is only
    /// removed with an explicit `confirmUserSidecar=true`. The file is only touched after the
    /// media-root and symlink checks pass, and the blocking unlink runs on `tokio::fs` so the core
    /// workers stay free.
    pub async fn delete_for_source(
        &self,
        item_id: &str,
        source_id: Option<&str>,
        confirm_user_sidecar: bool,
    ) -> Result<String, DanmakuServiceError> {
        let Some(tracked) = self
            .database
            .find_tracked_danmaku_source_for_item(item_id, source_id)
            .await?
        else {
            return Err(DanmakuServiceError::SidecarNotFound);
        };
        let is_user_sidecar = tracked
            .provider
            .as_deref()
            .is_none_or(|provider| provider.trim().is_empty());
        if is_user_sidecar && !confirm_user_sidecar {
            return Err(DanmakuServiceError::UserSidecarConfirmationRequired);
        }
        match safe_danmaku_source_paths(&tracked.root_path, &tracked.relative_path).await {
            Ok((_, sidecar_path)) => {
                self.remove_sidecar_file(&sidecar_path).await?;
            }
            // The path is unsafe: refuse and keep everything as it is.
            Err(DanmakuSourcePathError::UnsafePath) => {
                return Err(DanmakuServiceError::InvalidSidecarPath);
            }
            // The media file (or its root) is gone, so no file can be proven safe to unlink; the
            // registration itself can still be cleaned up.
            Err(DanmakuSourcePathError::Missing) => {}
        }
        self.database
            .delete_danmaku_track(&tracked.source_id)
            .await?;
        Ok(tracked.source_id)
    }

    /// Unlinks one canonical sidecar path, treating "already gone" as success.
    async fn remove_sidecar_file(&self, sidecar_path: &Path) -> Result<(), DanmakuServiceError> {
        match fs::symlink_metadata(sidecar_path).await {
            Ok(metadata)
                if metadata.file_type().is_symlink() || !metadata.file_type().is_file() =>
            {
                return Err(DanmakuServiceError::InvalidSidecarPath);
            }
            Ok(_) => {
                fs::remove_file(&sidecar_path)
                    .await
                    .map_err(|_| DanmakuServiceError::SidecarDeleteFailed)?;
            }
            // The file is already gone (for example an external cleanup or a rescan that marked the
            // track MISSING); removing the registration still has to succeed.
            Err(_) => {}
        }
        Ok(())
    }

    /// Resolves the media source a manual match operates on and rejects non-matchable items.
    async fn matchable_source(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<StoredDanmakuSource, DanmakuServiceError> {
        let source = self
            .database
            .find_danmaku_source_for_item(item_id, source_id)
            .await?
            .ok_or(DanmakuServiceError::SourceNotFound)?;
        if !source
            .item_type
            .as_deref()
            .is_some_and(|item_type| MATCHABLE_ITEM_TYPES.contains(&item_type))
        {
            return Err(DanmakuServiceError::UnsupportedItemType);
        }
        Ok(source)
    }
}

/// Maps a plugin error to the manual-match contract.
///
/// A plugin that never declared `danmaku.search`/`danmaku.fetch` is "unsupported" (it keeps
/// automatic matching), while a missing, disabled or not-yet-configured plugin is "unavailable".
/// Both end up as a `409 PLUGIN_UNAVAILABLE`, but the administrator sees which one it is.
fn manual_match_plugin_error(error: PluginServiceError) -> DanmakuServiceError {
    match error {
        PluginServiceError::Unsupported(_) => DanmakuServiceError::ManualMatchUnsupported,
        PluginServiceError::UnknownPlugin(_)
        | PluginServiceError::Unavailable(_)
        | PluginServiceError::InvalidConfig => DanmakuServiceError::ManualMatchUnavailable,
        other => DanmakuServiceError::Plugin(other),
    }
}

/// Title the search falls back to: the item's own title, or the series title for an episode.
fn default_search_query(source: &StoredDanmakuSource) -> Option<String> {
    let title = if source.item_type.as_deref() == Some("EPISODE") {
        source.series_title.as_deref().or(source.title.as_deref())
    } else {
        source.title.as_deref()
    }?;
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_owned())
}

/// Normalizes an optional identifier: `None` and a blank value both mean "not set".
fn optional_manual_identifier(
    value: Option<String>,
    max_chars: usize,
) -> Result<Option<String>, DanmakuServiceError> {
    match value {
        Some(value) if value.trim().is_empty() => Ok(None),
        Some(value) => normalized_manual_identifier(&value, max_chars)
            .map(Some)
            .ok_or(DanmakuServiceError::InvalidManualTarget),
        None => Ok(None),
    }
}

/// Trims an opaque provider identifier and enforces the shared length/character rules.
fn normalized_manual_identifier(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max_chars || value.chars().any(char::is_control)
    {
        return None;
    }
    Some(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{ManualMatchTarget, default_search_query, normalized_manual_identifier};
    use crate::storage::StoredDanmakuSource;

    fn source(item_type: &str) -> StoredDanmakuSource {
        StoredDanmakuSource {
            source_id: "source-1".to_owned(),
            root_path: "/media".to_owned(),
            relative_path: "Show/Episode 01.mkv".to_owned(),
            item_type: Some(item_type.to_owned()),
            season_number: Some(1),
            episode_number: Some(1),
            title: Some("第 1 集".to_owned()),
            original_title: Some("Episode 1".to_owned()),
            series_title: Some("示例剧名".to_owned()),
            series_original_title: Some("Example Show".to_owned()),
        }
    }

    #[test]
    fn the_default_query_uses_the_series_title_for_episodes() {
        assert_eq!(
            default_search_query(&source("EPISODE")).as_deref(),
            Some("示例剧名")
        );
        assert_eq!(
            default_search_query(&source("MOVIE")).as_deref(),
            Some("第 1 集")
        );
    }

    #[test]
    fn a_blank_provider_and_anime_id_mean_not_set() {
        let target = ManualMatchTarget {
            source_id: None,
            provider: Some("   ".to_owned()),
            anime_id: Some(String::new()),
            episode_id: " ep-1 ".to_owned(),
            overwrite: false,
        }
        .validated()
        .expect("blank optional identifiers are unset");
        assert_eq!(target.provider, None);
        assert_eq!(target.anime_id, None);
        assert_eq!(target.episode_id, "ep-1");
    }

    #[test]
    fn identifiers_are_bounded_and_reject_control_characters() {
        assert!(normalized_manual_identifier("   ", 128).is_none());
        assert!(normalized_manual_identifier(&"a".repeat(129), 128).is_none());
        assert!(normalized_manual_identifier("a\tb", 128).is_none());
        assert!(
            ManualMatchTarget {
                source_id: None,
                provider: None,
                anime_id: None,
                episode_id: String::new(),
                overwrite: false,
            }
            .validated()
            .is_err()
        );
    }
}
