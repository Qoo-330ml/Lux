use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

use crate::{
    domain::ids::UserId,
    storage::{
        Database, StorageError, UserPermissionCopyGroups as StoredPermissionCopyGroups,
        UserPermissionCopyResult as StoredPermissionCopyResult, UserPermissionCopySnapshot,
    },
};

pub(crate) const MAX_USER_PERMISSION_COPY_TARGETS: usize = 100;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UserPermissionCopyGroups {
    pub(crate) library: bool,
    pub(crate) content: bool,
    pub(crate) playback: bool,
}

impl UserPermissionCopyGroups {
    fn is_empty(self) -> bool {
        !self.library && !self.content && !self.playback
    }

    fn stored(self) -> StoredPermissionCopyGroups {
        StoredPermissionCopyGroups {
            library: self.library,
            content: self.content,
            playback: self.playback,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PreviewUserPermissionCopyInput {
    pub(crate) source_user_id: String,
    pub(crate) target_user_ids: Vec<String>,
    pub(crate) groups: UserPermissionCopyGroups,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PermissionCopyTargetVersionInput {
    pub(crate) user_id: String,
    pub(crate) expected_version: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ApplyUserPermissionCopyInput {
    pub(crate) source_user_id: String,
    pub(crate) source_version: String,
    pub(crate) targets: Vec<PermissionCopyTargetVersionInput>,
    pub(crate) groups: UserPermissionCopyGroups,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PermissionCopyPreview {
    pub(crate) source_user_id: String,
    pub(crate) source_version: String,
    pub(crate) source_snapshot: PermissionCopySnapshot,
    pub(crate) targets: Vec<PermissionCopyPreviewTarget>,
    pub(crate) groups: UserPermissionCopyGroups,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PermissionCopyPreviewTarget {
    pub(crate) user_id: String,
    pub(crate) expected_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PermissionCopySnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) library: Option<PermissionCopyLibrarySnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) content: Option<Option<PermissionCopyContentSnapshot>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) playback: Option<PermissionCopyPlaybackSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PermissionCopyLibrarySnapshot {
    pub(crate) access_mode: String,
    pub(crate) library_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PermissionCopyContentSnapshot {
    pub(crate) rating_system: String,
    pub(crate) maximum_rating: Option<String>,
    pub(crate) allow_unrated: bool,
    pub(crate) allowed_tags: Vec<String>,
    pub(crate) blocked_tags: Vec<String>,
    pub(crate) version: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PermissionCopyPlaybackSnapshot {
    pub(crate) can_download: bool,
    pub(crate) can_play: bool,
    pub(crate) max_simultaneous_playbacks: i64,
    pub(crate) allow_remux: bool,
    pub(crate) allow_audio_transcode: bool,
    pub(crate) allow_video_transcode: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum PermissionCopyErrorCode {
    UserUnavailable,
    VersionConflict,
    InvalidSnapshot,
    StorageUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PermissionCopyTargetResult {
    pub(crate) user_id: String,
    pub(crate) success: bool,
    pub(crate) error_code: Option<PermissionCopyErrorCode>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PermissionCopyBatchResult {
    pub(crate) source_user_id: String,
    pub(crate) source_version: String,
    pub(crate) results: Vec<PermissionCopyTargetResult>,
}

#[derive(Debug)]
pub(crate) enum PermissionCopyApplicationError {
    InvalidRequest,
    SourceUnavailable,
    UserUnavailable,
    SourceVersionConflict,
    InvalidSnapshot,
    Storage(StorageError),
}

impl fmt::Display for PermissionCopyApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest => formatter.write_str("user permission copy request is invalid"),
            Self::SourceUnavailable => formatter.write_str("source user is unavailable"),
            Self::UserUnavailable => formatter.write_str("target user is unavailable"),
            Self::SourceVersionConflict => {
                formatter.write_str("source permission snapshot changed")
            }
            Self::InvalidSnapshot => formatter.write_str("permission snapshot is invalid"),
            Self::Storage(error) => {
                let _ = error;
                formatter.write_str("user permission copy storage is unavailable")
            }
        }
    }
}

impl std::error::Error for PermissionCopyApplicationError {}

impl From<StorageError> for PermissionCopyApplicationError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

pub(crate) async fn preview_user_permission_copy(
    database: &Database,
    input: PreviewUserPermissionCopyInput,
) -> Result<PermissionCopyPreview, PermissionCopyApplicationError> {
    let source_user_id = normalize_user_id(&input.source_user_id)?;
    let target_user_ids =
        validate_target_ids(&source_user_id, &input.target_user_ids, input.groups)?;
    let mut user_ids = Vec::with_capacity(target_user_ids.len() + 1);
    user_ids.push(source_user_id.clone());
    user_ids.extend(target_user_ids.iter().cloned());
    let snapshots = database.user_permission_copy_snapshots(&user_ids).await?;
    let source = snapshots
        .get(&source_user_id)
        .ok_or(PermissionCopyApplicationError::SourceUnavailable)?;
    validate_source_snapshot(source, input.groups)?;
    let source_snapshot = copy_snapshot_view(source, input.groups);
    let source_version = snapshot_version(&source_user_id, &source_snapshot)?;
    let mut targets = Vec::with_capacity(target_user_ids.len());
    for user_id in target_user_ids {
        let snapshot = snapshots
            .get(&user_id)
            .ok_or(PermissionCopyApplicationError::UserUnavailable)?;
        let expected_version =
            snapshot_version(&user_id, &copy_snapshot_view(snapshot, input.groups))?;
        targets.push(PermissionCopyPreviewTarget {
            user_id,
            expected_version,
        });
    }

    Ok(PermissionCopyPreview {
        source_user_id,
        source_version,
        source_snapshot,
        targets,
        groups: input.groups,
    })
}

pub(crate) async fn apply_user_permission_copy(
    database: &Database,
    input: ApplyUserPermissionCopyInput,
) -> Result<PermissionCopyBatchResult, PermissionCopyApplicationError> {
    let source_user_id = normalize_user_id(&input.source_user_id)?;
    if !valid_snapshot_version(&input.source_version) {
        return Err(PermissionCopyApplicationError::InvalidRequest);
    }
    let target_user_ids = input
        .targets
        .iter()
        .map(|target| target.user_id.clone())
        .collect::<Vec<_>>();
    let target_user_ids = validate_target_ids(&source_user_id, &target_user_ids, input.groups)?;
    if input
        .targets
        .iter()
        .any(|target| !valid_snapshot_version(&target.expected_version))
    {
        return Err(PermissionCopyApplicationError::InvalidRequest);
    }

    let mut user_ids = Vec::with_capacity(target_user_ids.len() + 1);
    user_ids.push(source_user_id.clone());
    user_ids.extend(target_user_ids.iter().cloned());
    let snapshots = database.user_permission_copy_snapshots(&user_ids).await?;
    let source = snapshots
        .get(&source_user_id)
        .ok_or(PermissionCopyApplicationError::SourceUnavailable)?;
    validate_source_snapshot(source, input.groups)?;
    if snapshot_version(&source_user_id, &copy_snapshot_view(source, input.groups))?
        != input.source_version
    {
        return Err(PermissionCopyApplicationError::SourceVersionConflict);
    }

    // The snapshot reads above are already one bounded batch; this loop only performs the CAS
    // write, and each target keeps its own transaction so one failure cannot roll back another.
    // Merging the writes into a batch would merge those transactions and the per-target codes.
    let mut results = Vec::with_capacity(input.targets.len());
    for (target, target_user_id) in input.targets.iter().zip(target_user_ids.iter()) {
        let error_code = match snapshots.get(target_user_id) {
            None => Some(PermissionCopyErrorCode::UserUnavailable),
            Some(current) => {
                let current_version =
                    snapshot_version(target_user_id, &copy_snapshot_view(current, input.groups))?;
                if current_version != target.expected_version {
                    Some(PermissionCopyErrorCode::VersionConflict)
                } else {
                    match database
                        .copy_user_permission_groups(
                            &source_user_id,
                            source,
                            target_user_id,
                            current,
                            input.groups.stored(),
                        )
                        .await
                    {
                        Ok(StoredPermissionCopyResult::Updated) => None,
                        Ok(StoredPermissionCopyResult::Conflict) => {
                            Some(PermissionCopyErrorCode::VersionConflict)
                        }
                        Ok(StoredPermissionCopyResult::UserUnavailable) => {
                            Some(PermissionCopyErrorCode::UserUnavailable)
                        }
                        Ok(StoredPermissionCopyResult::InvalidSnapshot) => {
                            Some(PermissionCopyErrorCode::InvalidSnapshot)
                        }
                        Err(_) => Some(PermissionCopyErrorCode::StorageUnavailable),
                    }
                }
            }
        };
        results.push(PermissionCopyTargetResult {
            user_id: target_user_id.clone(),
            success: error_code.is_none(),
            error_code,
        });
    }

    Ok(PermissionCopyBatchResult {
        source_user_id,
        source_version: input.source_version,
        results,
    })
}

fn normalize_user_id(value: &str) -> Result<String, PermissionCopyApplicationError> {
    value
        .parse::<UserId>()
        .map(|user_id| user_id.to_string())
        .map_err(|_| PermissionCopyApplicationError::InvalidRequest)
}

fn validate_target_ids(
    source_user_id: &str,
    target_user_ids: &[String],
    groups: UserPermissionCopyGroups,
) -> Result<Vec<String>, PermissionCopyApplicationError> {
    if groups.is_empty()
        || target_user_ids.is_empty()
        || target_user_ids.len() > MAX_USER_PERMISSION_COPY_TARGETS
    {
        return Err(PermissionCopyApplicationError::InvalidRequest);
    }
    let mut seen = std::collections::HashSet::with_capacity(target_user_ids.len());
    let mut normalized = Vec::with_capacity(target_user_ids.len());
    for target_user_id in target_user_ids {
        let user_id = normalize_user_id(target_user_id)?;
        if user_id == source_user_id || !seen.insert(user_id.clone()) {
            return Err(PermissionCopyApplicationError::InvalidRequest);
        }
        normalized.push(user_id);
    }
    Ok(normalized)
}

fn valid_snapshot_version(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_source_snapshot(
    snapshot: &UserPermissionCopySnapshot,
    groups: UserPermissionCopyGroups,
) -> Result<(), PermissionCopyApplicationError> {
    if groups.library {
        let valid_library_state = match snapshot.library_access_mode.as_str() {
            "ALL" | "NONE" => snapshot.library_ids.is_empty(),
            "SELECTED" => !snapshot.library_ids.is_empty(),
            _ => false,
        };
        if !valid_library_state {
            return Err(PermissionCopyApplicationError::InvalidSnapshot);
        }
    }
    if groups.playback && snapshot.playback_policy.max_simultaneous_playbacks < 0 {
        return Err(PermissionCopyApplicationError::InvalidSnapshot);
    }
    Ok(())
}

fn copy_snapshot_view(
    snapshot: &UserPermissionCopySnapshot,
    groups: UserPermissionCopyGroups,
) -> PermissionCopySnapshot {
    PermissionCopySnapshot {
        library: groups.library.then(|| PermissionCopyLibrarySnapshot {
            access_mode: snapshot.library_access_mode.clone(),
            library_ids: snapshot.library_ids.clone(),
        }),
        content: groups.content.then(|| {
            snapshot
                .content_restrictions
                .as_ref()
                .map(|policy| PermissionCopyContentSnapshot {
                    rating_system: policy.rating_system.clone(),
                    maximum_rating: policy.maximum_rating.clone(),
                    allow_unrated: policy.allow_unrated,
                    allowed_tags: policy.allowed_tags.clone(),
                    blocked_tags: policy.blocked_tags.clone(),
                    version: policy.version,
                })
        }),
        playback: groups.playback.then_some(PermissionCopyPlaybackSnapshot {
            can_download: snapshot.can_download,
            can_play: snapshot.playback_policy.can_play,
            max_simultaneous_playbacks: snapshot.playback_policy.max_simultaneous_playbacks,
            allow_remux: snapshot.playback_policy.allow_remux,
            allow_audio_transcode: snapshot.playback_policy.allow_audio_transcode,
            allow_video_transcode: snapshot.playback_policy.allow_video_transcode,
        }),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PermissionSnapshotVersion<'a> {
    user_id: &'a str,
    snapshot: &'a PermissionCopySnapshot,
}

fn snapshot_version(
    user_id: &str,
    snapshot: &PermissionCopySnapshot,
) -> Result<String, PermissionCopyApplicationError> {
    let serialized = serde_json::to_vec(&PermissionSnapshotVersion { user_id, snapshot })
        .map_err(|_| PermissionCopyApplicationError::InvalidRequest)?;
    Ok(format!("{:x}", Sha256::digest(serialized)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_database;
    use crate::{
        application::libraries::LibraryService,
        domain::ids::UserId,
        library::LibraryKind,
        storage::{
            Database, NewUserContentRestrictions, UserContentRestrictionsCasResult,
            UserPlaybackPolicy,
        },
    };

    async fn insert_user(database: &Database, id: &str) -> Result<(), Box<dyn std::error::Error>> {
        database
            .insert_user(id, id, id, "test", false, true)
            .await?;
        Ok(())
    }

    #[test]
    fn snapshot_versions_are_bound_to_user_id() -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = PermissionCopySnapshot {
            library: None,
            content: None,
            playback: Some(PermissionCopyPlaybackSnapshot {
                can_download: true,
                can_play: true,
                max_simultaneous_playbacks: 2,
                allow_remux: true,
                allow_audio_transcode: true,
                allow_video_transcode: true,
            }),
        };

        assert_ne!(
            snapshot_version("user-a", &snapshot)?,
            snapshot_version("user-b", &snapshot)?
        );
        Ok(())
    }

    #[tokio::test]
    async fn previews_selected_permission_groups_and_reports_per_target_conflicts()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let source_id = UserId::new().to_string();
        let success_target_id = UserId::new().to_string();
        let stale_target_id = UserId::new().to_string();
        for id in [&source_id, &success_target_id, &stale_target_id] {
            insert_user(&database, id).await?;
        }
        let library = LibraryService::new(database.clone())
            .create_library("Permission Copy App", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        database
            .replace_user_library_access_mode(
                &source_id,
                "SELECTED",
                std::slice::from_ref(&library_id),
            )
            .await?;
        database
            .update_user_playback_policy(
                &source_id,
                UserPlaybackPolicy {
                    can_play: true,
                    max_simultaneous_playbacks: 4,
                    allow_remux: true,
                    allow_audio_transcode: true,
                    allow_video_transcode: false,
                },
            )
            .await?;
        let allowed_tags = vec!["Family".to_owned()];
        let blocked_tags = vec!["Horror".to_owned()];
        assert!(matches!(
            database
                .compare_and_swap_user_content_restrictions(
                    &source_id,
                    0,
                    NewUserContentRestrictions {
                        rating_system: "MPA",
                        maximum_rating: Some("PG-13"),
                        allow_unrated: false,
                        allowed_tags: &allowed_tags,
                        blocked_tags: &blocked_tags,
                    },
                )
                .await?,
            UserContentRestrictionsCasResult::Updated(_)
        ));

        let preview = preview_user_permission_copy(
            &database,
            PreviewUserPermissionCopyInput {
                source_user_id: source_id.clone(),
                target_user_ids: vec![success_target_id.clone(), stale_target_id.clone()],
                groups: UserPermissionCopyGroups {
                    library: true,
                    content: true,
                    playback: false,
                },
            },
        )
        .await?;

        assert!(preview.source_snapshot.library.is_some());
        assert!(preview.source_snapshot.content.is_some());
        assert!(preview.source_snapshot.playback.is_none());
        assert_eq!(preview.targets.len(), 2);
        assert_eq!(preview.targets[0].expected_version.len(), 64);

        database
            .replace_user_library_access_mode(&stale_target_id, "NONE", &[])
            .await?;
        let result = apply_user_permission_copy(
            &database,
            ApplyUserPermissionCopyInput {
                source_user_id: preview.source_user_id,
                source_version: preview.source_version,
                targets: preview
                    .targets
                    .into_iter()
                    .map(|target| PermissionCopyTargetVersionInput {
                        user_id: target.user_id,
                        expected_version: target.expected_version,
                    })
                    .collect(),
                groups: preview.groups,
            },
        )
        .await?;

        assert!(result.results[0].success);
        assert_eq!(result.results[0].error_code, None);
        assert!(!result.results[1].success);
        assert_eq!(
            result.results[1].error_code,
            Some(PermissionCopyErrorCode::VersionConflict)
        );
        let targets = database
            .user_permission_copy_snapshots(&[success_target_id.clone(), stale_target_id.clone()])
            .await?;
        assert_eq!(targets[&success_target_id].library_access_mode, "SELECTED");
        assert_eq!(targets[&success_target_id].library_ids, [library_id]);
        assert_eq!(
            targets[&success_target_id]
                .content_restrictions
                .as_ref()
                .map(|policy| policy.rating_system.as_str()),
            Some("MPA")
        );
        assert_eq!(targets[&stale_target_id].library_access_mode, "NONE");
        Ok(())
    }

    #[tokio::test]
    async fn rejects_stale_source_before_modifying_any_target()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let source_id = UserId::new().to_string();
        let target_id = UserId::new().to_string();
        insert_user(&database, &source_id).await?;
        insert_user(&database, &target_id).await?;
        database
            .update_user_playback_policy(
                &source_id,
                UserPlaybackPolicy {
                    can_play: true,
                    max_simultaneous_playbacks: 4,
                    allow_remux: true,
                    allow_audio_transcode: false,
                    allow_video_transcode: false,
                },
            )
            .await?;
        database
            .update_user_playback_policy(
                &target_id,
                UserPlaybackPolicy {
                    can_play: false,
                    max_simultaneous_playbacks: 1,
                    allow_remux: false,
                    allow_audio_transcode: false,
                    allow_video_transcode: false,
                },
            )
            .await?;
        let preview = preview_user_permission_copy(
            &database,
            PreviewUserPermissionCopyInput {
                source_user_id: source_id.clone(),
                target_user_ids: vec![target_id.clone()],
                groups: UserPermissionCopyGroups {
                    library: false,
                    content: false,
                    playback: true,
                },
            },
        )
        .await?;
        database
            .update_user_playback_policy(
                &source_id,
                UserPlaybackPolicy {
                    can_play: true,
                    max_simultaneous_playbacks: 6,
                    allow_remux: true,
                    allow_audio_transcode: false,
                    allow_video_transcode: false,
                },
            )
            .await?;

        assert!(matches!(
            apply_user_permission_copy(
                &database,
                ApplyUserPermissionCopyInput {
                    source_user_id: preview.source_user_id,
                    source_version: preview.source_version,
                    targets: preview
                        .targets
                        .into_iter()
                        .map(|target| PermissionCopyTargetVersionInput {
                            user_id: target.user_id,
                            expected_version: target.expected_version,
                        })
                        .collect(),
                    groups: preview.groups,
                },
            )
            .await,
            Err(PermissionCopyApplicationError::SourceVersionConflict)
        ));
        let target = database
            .user_permission_copy_snapshots(std::slice::from_ref(&target_id))
            .await?;
        assert!(!target[&target_id].playback_policy.can_play);
        assert_eq!(
            target[&target_id]
                .playback_policy
                .max_simultaneous_playbacks,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn rejects_invalid_copy_scope_ids_and_version_tokens_before_storage()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let source_id = UserId::new().to_string();
        let target_id = UserId::new().to_string();
        insert_user(&database, &source_id).await?;
        insert_user(&database, &target_id).await?;
        let groups = UserPermissionCopyGroups {
            library: false,
            content: false,
            playback: true,
        };

        for input in [
            PreviewUserPermissionCopyInput {
                source_user_id: source_id.clone(),
                target_user_ids: vec![target_id.clone()],
                groups: UserPermissionCopyGroups {
                    library: false,
                    content: false,
                    playback: false,
                },
            },
            PreviewUserPermissionCopyInput {
                source_user_id: source_id.clone(),
                target_user_ids: vec![source_id.clone()],
                groups,
            },
            PreviewUserPermissionCopyInput {
                source_user_id: source_id.clone(),
                target_user_ids: vec![target_id.clone(), target_id.clone()],
                groups,
            },
            PreviewUserPermissionCopyInput {
                source_user_id: source_id.clone(),
                target_user_ids: vec!["not-a-uuid".to_owned()],
                groups,
            },
            PreviewUserPermissionCopyInput {
                source_user_id: source_id.clone(),
                target_user_ids: (0..=MAX_USER_PERMISSION_COPY_TARGETS)
                    .map(|_| UserId::new().to_string())
                    .collect(),
                groups,
            },
        ] {
            database.reset_query_count();
            assert!(matches!(
                preview_user_permission_copy(&database, input).await,
                Err(PermissionCopyApplicationError::InvalidRequest)
            ));
            assert_eq!(database.query_count(), 0);
        }

        database.reset_query_count();
        assert!(matches!(
            apply_user_permission_copy(
                &database,
                ApplyUserPermissionCopyInput {
                    source_user_id: source_id,
                    source_version: "g".repeat(64),
                    targets: vec![PermissionCopyTargetVersionInput {
                        user_id: target_id,
                        expected_version: "not-a-version".to_owned(),
                    }],
                    groups,
                },
            )
            .await,
            Err(PermissionCopyApplicationError::InvalidRequest)
        ));
        assert_eq!(database.query_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn previews_an_explicit_null_when_selected_source_has_no_content_policy()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let source_id = UserId::new().to_string();
        let target_id = UserId::new().to_string();
        insert_user(&database, &source_id).await?;
        insert_user(&database, &target_id).await?;
        let input = PreviewUserPermissionCopyInput {
            source_user_id: source_id,
            target_user_ids: vec![target_id],
            groups: UserPermissionCopyGroups {
                library: false,
                content: true,
                playback: false,
            },
        };

        let preview = preview_user_permission_copy(&database, input.clone()).await?;
        let repeated_preview = preview_user_permission_copy(&database, input).await?;
        let serialized = serde_json::to_value(&preview.source_snapshot)?;
        assert_eq!(serialized["content"], serde_json::Value::Null);
        assert!(serialized.get("library").is_none());
        assert!(serialized.get("playback").is_none());
        assert_eq!(preview.source_version, repeated_preview.source_version);
        assert_eq!(
            preview.targets[0].expected_version,
            repeated_preview.targets[0].expected_version
        );
        Ok(())
    }
}
