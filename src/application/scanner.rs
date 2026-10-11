use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use quick_xml::{events::Event, reader::Reader};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use tokio::{
    fs,
    io::AsyncReadExt,
    sync::{Notify, OwnedSemaphorePermit, Semaphore, watch},
    task::{JoinHandle, JoinSet},
};
use uuid::Uuid;

use crate::{
    application::{
        admin_events::{AdminEventHub, UserEventHub},
        candidates::MetadataSelectionService,
        home::HomeService,
        library_covers::{AutoLibraryCoverResult, LibraryCoverService},
        media_matching::{
            MediaKind, clean_title, has_multi_part_marker, has_source_variant_marker,
            parse_media_name, parse_media_name_with_variant_suffix,
        },
        metadata::{MetadataEnricher, ScanLocalMetadataNfoBatch},
        nfo::LocalNfoMetadataStore,
        people::PeopleService,
        probe::MediaProbeService,
        reidentify::{MetadataRefreshMode, MetadataReidentifyError, MetadataReidentifyService},
        strm_probe::StrmProbeService,
        strm_target::{StrmTarget, StrmTargetKind, classify_strm_target},
        thumbnails::ThumbnailService,
        watch::ChangeKind,
        webhooks::{WebhookEventType, WebhookService},
    },
    config::{
        DEFAULT_SCAN_CONCURRENCY, scan_concurrency_from_env, scan_concurrency_override_from_env,
    },
    domain::ids::{FilesystemEntryId, ItemId, LibraryId, SourceId},
    observability::{logs::LogStore, resources::ResourceMetrics},
    storage::{
        Database, FilesystemEntryMove, MANIFEST_POSTPROCESSING_TARGET_PAGE_SIZE,
        ManifestDeltaBatchCommit, ManifestDiscoveryCommitResult, ManifestPostprocessingTargetPage,
        MetadataAutoMatchPolicy, NewEpisodeFile, NewFilesystemEntry, NewHierarchyItem,
        NewItemMetadataCompletenessResult, NewMediaItem, NewMediaSource, NewMovieFile,
        NewScanManifest, NewScanManifestDelta, NewScanManifestDiscoveryChunk, NewScanManifestEntry,
        NewScanManifestIndexedFile, NewScanManifestPositiveIndex, NewScanManifestRoot,
        NewScanManifestSeenFilesystemEntry, NewScanManifestUnresolvedFile,
        ReconciliationBatchCommit, StorageError, StoredEpisodeIdentityCandidate,
        StoredFilesystemEntry, StoredLibraryRoot, StoredReconciliationScanEntry, StoredScanJob,
        StoredScanJobPath, StoredScanLocalMetadataBackfillPage, StoredScanLocalMetadataBatch,
        StoredScanLocalMetadataSource, is_lite_manifest_discovery, movie_parent_folder_identity,
    },
};

const FILE_BATCH_SIZE: usize = 500;
pub const BACKGROUND_SCAN_BATCH_SIZE: usize = 100;
const MANIFEST_APPLY_BATCH_SIZE: usize = 500;
const MAX_SCAN_JOB_BATCH_SIZE: usize = 500;
const DISCOVERY_BATCH_SIZE: usize = 16;
// A typical fixture directory contains about 100 files. Keep one discovery
// work unit close to the 8,000-file streamed index budget without changing
// the inner file and in-flight entry caps.
const MANIFEST_DISCOVERY_BATCH_SIZE: usize = 80;
const MANIFEST_DIFF_BATCH_SIZE: usize = 500;
const MANIFEST_REMOVAL_DIRECTORY_BATCH_SIZE: i64 = 128;
const FINGERPRINT_CHECK_CONCURRENCY: usize = 64;
const DISCOVERY_ENTRY_BATCH_SIZE: usize = 1024;
const MANIFEST_STREAMED_INDEX_BATCH_SIZE: usize = 8_000;
const MANIFEST_STREAMED_ENTRY_BATCH_SIZE: usize = 8_192;
const DISCOVERY_CHILD_DIRECTORY_BATCH_SIZE: usize = 200;
const MAX_STRM_TARGET_BYTES: usize = 1024 * 1024;

mod jobs;
mod library;
mod manifest;
mod names;

use jobs::{
    ActiveScanJobRun, LocalMetadataWorkerHandle, ScanJobRunGuard, ScanRunRegistry,
    notify_local_metadata_images_waiter,
};
pub use jobs::{
    IncrementalScanChange, LibraryDeletionGuard, LocalMetadataRefreshAccepted, ScanBatchReport,
    ScanJob, ScanJobError, ScanJobService,
};
pub use library::LibraryScanner;
use library::{FileBatchWalker, is_strm_file, is_supported_movie_file, is_supported_sidecar_file};
use manifest::*;
use names::{
    EpisodeHierarchy, ManifestFilenameInput, MixedClassification, MixedClassificationCache,
    PreparedManifestFilename, classify_mixed_file, classify_mixed_manifest_file,
    clean_hierarchy_title, episode_hierarchy, infer_sibling_movie_variant_suffix,
    legacy_series_identity, merge_movie_provider_ids, movie_folder_provider_ids,
    movie_provider_ids, movie_variant_extensions, parse_movie_filename_with_variant_suffix,
    prepare_manifest_filename, prepare_manifest_filename_with_variant_suffix, provider_ids_json,
};
pub use names::{
    ParsedEpisodeFilename, ParsedMovieFilename, parse_episode_filename, parse_movie_filename,
};

const MISSING_ENTRY_BATCH_SIZE: usize = 500;
const LOCAL_METADATA_IDLE_FALLBACK: Duration = Duration::from_secs(1);
const LOCAL_METADATA_COMPLETION_FALLBACK: Duration = Duration::from_secs(5);
const LOCAL_METADATA_JOB_REFRESH_FALLBACK: Duration = Duration::from_secs(5);
const LOCAL_METADATA_BATCH_SIZE: usize = 16;
const LIBRARY_DELETION_SCAN_CANCEL_TIMEOUT: Duration = Duration::from_secs(30);
const SQLITE_SCAN_LOCK_RETRY_DELAYS_MS: [u64; 3] = [250, 750, 1_500];
const SCAN_LOCAL_METADATA_OUTBOX_IDLE_FALLBACK: Duration = Duration::from_millis(250);
const SCAN_LOCAL_METADATA_RETRY_DELAY_SECONDS: i64 = 30;
const MAX_SCAN_LOCAL_METADATA_SOURCE_IDS: usize = 256;
const SCAN_LOCAL_METADATA_BACKFILL_PAGE_SIZE: usize = 16;
const SCAN_LOCAL_METADATA_COMPLETENESS_CHECK_BATCH_SIZE: usize = 512;
const MAX_SCAN_LOCAL_METADATA_NFO_BATCHES_IN_FLIGHT: usize = 4;
const SCAN_LOCK_DATABASE_WAIT_BACKOFF_MS: [u64; 6] = [10, 20, 40, 80, 160, 250];

fn scan_lock_database_wait_backoff(retry_number: usize) -> Duration {
    let delay_index = retry_number.min(SCAN_LOCK_DATABASE_WAIT_BACKOFF_MS.len() - 1);
    Duration::from_millis(SCAN_LOCK_DATABASE_WAIT_BACKOFF_MS[delay_index])
}

struct LocalMetadataCompletionContext<'a> {
    metadata_selection: Option<&'a MetadataSelectionService>,
    metadata_reidentify: Option<&'a MetadataReidentifyService>,
    home: Option<&'a HomeService>,
    user_events: &'a UserEventHub,
}

async fn process_scan_local_metadata_batch(
    database: &Database,
    enricher: &MetadataEnricher,
    home: Option<&HomeService>,
    user_events: &UserEventHub,
    batch: StoredScanLocalMetadataBatch,
) -> Option<(
    String,
    String,
    Vec<StoredScanLocalMetadataSource>,
    Vec<String>,
)> {
    let batch_id = batch.id.clone();
    let scan_job_id = batch.job_id.clone();
    let source_ids = match serde_json::from_str::<Vec<String>>(&batch.source_refs_json) {
        Ok(source_ids)
            if source_ids.len() == usize::try_from(batch.source_count).unwrap_or(usize::MAX)
                && !source_ids.is_empty()
                && source_ids.len() <= MAX_SCAN_LOCAL_METADATA_SOURCE_IDS
                && source_ids
                    .iter()
                    .all(|source_id| !source_id.trim().is_empty())
                && source_ids.iter().collect::<HashSet<_>>().len() == source_ids.len() =>
        {
            source_ids
        }
        _ => {
            fail_scan_local_metadata_batch(database, &batch_id, "invalid source reference batch")
                .await;
            return None;
        }
    };
    let non_retryable_item_ids =
        match serde_json::from_str::<Vec<String>>(&batch.non_retryable_item_ids_json) {
            Ok(item_ids)
                if item_ids.len() <= MAX_SCAN_LOCAL_METADATA_SOURCE_IDS
                    && item_ids.iter().all(|item_id| !item_id.trim().is_empty())
                    && item_ids.iter().collect::<HashSet<_>>().len() == item_ids.len() =>
            {
                item_ids
            }
            _ => {
                fail_scan_local_metadata_batch(database, &batch_id, "invalid NFO exclusion list")
                    .await;
                return None;
            }
        };

    let result = if batch.images_completed_at.is_some() {
        database
            .list_scan_local_metadata_sources(&source_ids)
            .await
            .map_err(|error| error.to_string())
    } else {
        match enricher
            .index_scan_local_metadata_batch_images(&source_ids)
            .await
        {
            Ok(batch) if !batch.report.failed_item_ids.is_empty() => Err(format!(
                "{} local image item(s) failed",
                batch.report.failed_item_ids.len()
            )),
            Ok(batch) => match database
                .mark_scan_local_metadata_images_complete(&batch_id)
                .await
            {
                Ok(true) => Ok(batch.sources),
                Ok(false) => Err("local metadata batch stopped before image completion".to_owned()),
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error.to_string()),
        }
    };
    if let Some(home) = home {
        home.invalidate();
    }
    user_events.publish_home_coalesced().await;

    match result {
        Ok(source_snapshot) => Some((
            batch_id,
            scan_job_id,
            source_snapshot,
            non_retryable_item_ids,
        )),
        Err(error) => {
            tracing::warn!(batch_id, %error, "local image batch failed and will be retried");
            fail_scan_local_metadata_batch(database, &batch_id, &error).await;
            None
        }
    }
}

async fn finish_scan_local_metadata_batch(
    database: &Database,
    enricher: &MetadataEnricher,
    context: LocalMetadataCompletionContext<'_>,
    batch_id: &str,
    scan_job_id: &str,
    source_snapshot: Vec<StoredScanLocalMetadataSource>,
    existing_non_retryable_item_ids: &[String],
) {
    let mut non_retryable_item_ids = existing_non_retryable_item_ids.to_vec();
    let result = match enricher
        .enrich_scan_local_metadata_batch_nfo(source_snapshot, &non_retryable_item_ids)
        .await
    {
        Ok(ScanLocalMetadataNfoBatch {
            report,
            source_identities,
        }) => {
            for item_id in &report.non_retryable_failed_item_ids {
                if !non_retryable_item_ids.contains(item_id) {
                    non_retryable_item_ids.push(item_id.clone());
                }
            }
            let retryable_failed_item_count = report.retryable_failed_item_count();
            if retryable_failed_item_count > 0 {
                Err(format!(
                    "{retryable_failed_item_count} local NFO item(s) failed"
                ))
            } else {
                let issue = (!non_retryable_item_ids.is_empty()).then(|| {
                    format!(
                        "{} non-retryable local NFO item(s) failed",
                        non_retryable_item_ids.len()
                    )
                });
                complete_local_metadata_completeness(
                    database,
                    context.metadata_selection,
                    context.metadata_reidentify,
                    LocalMetadataCompletenessTrigger::ScanJob(scan_job_id),
                    &source_identities,
                    &non_retryable_item_ids,
                    context.user_events,
                )
                .await
                .map(|()| issue)
            }
        }
        Err(error) => Err(error.to_string()),
    };
    if let Some(home) = context.home {
        home.invalidate();
    }
    context.user_events.publish_home_coalesced().await;

    match result {
        Ok(issue) => match database
            .complete_scan_local_metadata_batch_with_outcome(
                batch_id,
                issue.as_deref(),
                &non_retryable_item_ids,
            )
            .await
        {
            Ok(true) => {}
            Ok(false) => tracing::debug!(batch_id, "local metadata batch was already terminal"),
            Err(error) => {
                tracing::warn!(batch_id, %error, "local metadata batch completion could not be saved");
                fail_scan_local_metadata_batch_with_non_retryable_item_ids(
                    database,
                    batch_id,
                    "local metadata completion failed",
                    &non_retryable_item_ids,
                )
                .await;
            }
        },
        Err(error) => {
            tracing::warn!(batch_id, %error, "local NFO batch failed and will be retried");
            fail_scan_local_metadata_batch_with_non_retryable_item_ids(
                database,
                batch_id,
                &error,
                &non_retryable_item_ids,
            )
            .await;
        }
    }
}

async fn fail_scan_local_metadata_batch(database: &Database, batch_id: &str, error: &str) {
    let retry_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
        .checked_add(SCAN_LOCAL_METADATA_RETRY_DELAY_SECONDS)
        .unwrap_or(i64::MAX);
    if let Err(storage_error) = database
        .fail_scan_local_metadata_batch(batch_id, error, Some(retry_at))
        .await
    {
        tracing::warn!(batch_id, %storage_error, "local metadata batch retry could not be saved");
    }
}

async fn fail_scan_local_metadata_batch_with_non_retryable_item_ids(
    database: &Database,
    batch_id: &str,
    error: &str,
    non_retryable_item_ids: &[String],
) {
    let retry_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
        .checked_add(SCAN_LOCAL_METADATA_RETRY_DELAY_SECONDS)
        .unwrap_or(i64::MAX);
    if let Err(storage_error) = database
        .fail_scan_local_metadata_batch_with_non_retryable_item_ids(
            batch_id,
            error,
            non_retryable_item_ids,
            Some(retry_at),
        )
        .await
    {
        tracing::warn!(batch_id, %storage_error, "local metadata batch retry could not be saved");
    }
}

async fn process_scan_local_metadata_backfill_images(
    enricher: &MetadataEnricher,
    home: Option<&HomeService>,
    user_events: &UserEventHub,
    page: &StoredScanLocalMetadataBackfillPage,
) -> Result<Vec<StoredScanLocalMetadataSource>, String> {
    let result = enricher
        .index_scan_local_metadata_batch_images(&page.entry_ids)
        .await;
    if let Some(home) = home {
        home.invalidate();
    }
    user_events.publish_home_coalesced().await;
    let batch = result.map_err(|error| error.to_string())?;
    if !batch.report.failed_item_ids.is_empty() {
        return Err(format!(
            "{} local image item(s) failed",
            batch.report.failed_item_ids.len()
        ));
    }
    Ok(batch.sources)
}

async fn finish_scan_local_metadata_backfill_page(
    database: &Database,
    enricher: &MetadataEnricher,
    context: LocalMetadataCompletionContext<'_>,
    page: StoredScanLocalMetadataBackfillPage,
    source_snapshot: Vec<StoredScanLocalMetadataSource>,
) {
    let mut non_retryable_item_ids = page.non_retryable_item_ids.clone();
    let result = match enricher
        .enrich_scan_local_metadata_batch_nfo(source_snapshot, &page.non_retryable_item_ids)
        .await
    {
        Ok(ScanLocalMetadataNfoBatch {
            report,
            source_identities,
        }) => {
            for item_id in &report.non_retryable_failed_item_ids {
                if !non_retryable_item_ids.contains(item_id) {
                    non_retryable_item_ids.push(item_id.clone());
                }
            }
            let retryable_failed_item_count = report.retryable_failed_item_count();
            if retryable_failed_item_count > 0 {
                Err(format!(
                    "{retryable_failed_item_count} local NFO item(s) failed"
                ))
            } else {
                let issue = (!non_retryable_item_ids.is_empty()).then(|| {
                    format!(
                        "{} non-retryable local NFO item(s) failed",
                        non_retryable_item_ids.len()
                    )
                });
                complete_local_metadata_completeness(
                    database,
                    context.metadata_selection,
                    context.metadata_reidentify,
                    LocalMetadataCompletenessTrigger::LibrarySetting,
                    &source_identities,
                    &non_retryable_item_ids,
                    context.user_events,
                )
                .await
                .map(|()| issue)
            }
        }
        Err(error) => Err(error.to_string()),
    };
    if let Some(home) = context.home {
        home.invalidate();
    }
    context.user_events.publish_home_coalesced().await;

    match result {
        Ok(issue) => match database
            .complete_scan_local_metadata_backfill_page_with_optional_issue(&page, issue.as_deref())
            .await
        {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                library_root_id = %page.library_root_id,
                "local metadata backfill page was already terminal or changed"
            ),
            Err(error) => {
                tracing::warn!(
                    library_root_id = %page.library_root_id,
                    %error,
                    "local metadata backfill page completion could not be saved"
                );
                fail_scan_local_metadata_backfill_page(
                    database,
                    &page,
                    "local metadata backfill completion failed",
                    &non_retryable_item_ids,
                )
                .await;
            }
        },
        Err(error) => {
            tracing::warn!(
                library_root_id = %page.library_root_id,
                %error,
                "local metadata backfill NFO failed and will be retried"
            );
            fail_scan_local_metadata_backfill_page(
                database,
                &page,
                &error,
                &non_retryable_item_ids,
            )
            .await;
        }
    }
}

async fn fail_scan_local_metadata_backfill_page(
    database: &Database,
    page: &StoredScanLocalMetadataBackfillPage,
    error: &str,
    non_retryable_item_ids: &[String],
) {
    let retry_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
        .checked_add(SCAN_LOCAL_METADATA_RETRY_DELAY_SECONDS)
        .unwrap_or(i64::MAX);
    if let Err(storage_error) = database
        .fail_scan_local_metadata_backfill_page_with_exclusions(
            page,
            error,
            non_retryable_item_ids,
            Some(retry_at),
        )
        .await
    {
        tracing::warn!(
            library_root_id = %page.library_root_id,
            %storage_error,
            "local metadata backfill retry could not be saved"
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LocalMetadataCompletenessTrigger<'a> {
    ScanJob(&'a str),
    ScanJobPolicy(MetadataAutoMatchPolicy),
    LibrarySetting,
}

fn metadata_auto_match_policy_for_scan_job(job: Option<&StoredScanJob>) -> MetadataAutoMatchPolicy {
    metadata_auto_match_policy_for_scan_job_snapshot(
        job.map(|job| job.job_type.as_str()),
        job.is_some_and(|job| job.auto_metadata_match),
    )
}

fn metadata_auto_match_policy_for_scan_job_snapshot(
    job_type: Option<&str>,
    auto_metadata_match: bool,
) -> MetadataAutoMatchPolicy {
    match job_type {
        Some("INCREMENTAL_SCAN") => {
            if auto_metadata_match {
                MetadataAutoMatchPolicy::Enabled
            } else {
                MetadataAutoMatchPolicy::Disabled
            }
        }
        Some(_) => MetadataAutoMatchPolicy::UseLibrarySetting,
        None => MetadataAutoMatchPolicy::Disabled,
    }
}

async fn resolve_local_metadata_auto_match_policy(
    database: &Database,
    trigger: LocalMetadataCompletenessTrigger<'_>,
) -> Result<MetadataAutoMatchPolicy, String> {
    match trigger {
        LocalMetadataCompletenessTrigger::LibrarySetting => {
            Ok(MetadataAutoMatchPolicy::UseLibrarySetting)
        }
        LocalMetadataCompletenessTrigger::ScanJobPolicy(policy) => Ok(policy),
        LocalMetadataCompletenessTrigger::ScanJob(scan_job_id) => {
            let job = database
                .find_scan_job(scan_job_id)
                .await
                .map_err(|error| error.to_string())?;
            Ok(metadata_auto_match_policy_for_scan_job(job.as_ref()))
        }
    }
}

type PendingLocalMetadataCompletenessCheck = (String, String, Vec<u8>, bool, bool);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScraperAvailabilityPreflight {
    Available,
    Unavailable,
    Failed,
}

impl ScraperAvailabilityPreflight {
    fn from_check_result(result: Result<Option<bool>, ()>) -> Self {
        match result {
            Ok(Some(true)) => Self::Available,
            Ok(Some(false)) | Ok(None) => Self::Unavailable,
            Err(()) => Self::Failed,
        }
    }

    const fn allows_fill_missing(self) -> bool {
        matches!(self, Self::Available | Self::Failed)
    }
}

async fn complete_local_metadata_completeness(
    database: &Database,
    selection: Option<&MetadataSelectionService>,
    metadata_reidentify: Option<&MetadataReidentifyService>,
    trigger: LocalMetadataCompletenessTrigger<'_>,
    source_identities: &[(String, String)],
    excluded_item_ids: &[String],
    user_events: &UserEventHub,
) -> Result<(), String> {
    if selection.is_none() {
        return Ok(());
    }
    let source_identities = source_identities
        .iter()
        .filter(|(item_id, _)| !excluded_item_ids.iter().any(|excluded| excluded == item_id))
        .cloned()
        .collect::<Vec<_>>();
    let mut item_ids = database
        .list_current_scan_local_metadata_item_ids(&source_identities)
        .await
        .map_err(|error| error.to_string())?;
    item_ids.sort_unstable();
    item_ids.dedup();
    complete_local_metadata_completeness_for_item_ids(
        database,
        selection,
        metadata_reidentify,
        trigger,
        &item_ids,
        user_events,
    )
    .await
}

async fn complete_local_metadata_completeness_for_item_ids(
    database: &Database,
    selection: Option<&MetadataSelectionService>,
    metadata_reidentify: Option<&MetadataReidentifyService>,
    trigger: LocalMetadataCompletenessTrigger<'_>,
    item_ids: &[String],
    user_events: &UserEventHub,
) -> Result<(), String> {
    let Some(selection) = selection else {
        return Ok(());
    };
    let auto_match_policy = resolve_local_metadata_auto_match_policy(database, trigger).await?;
    let metadata_by_item = database
        .list_active_media_item_metadata_with_libraries(item_ids)
        .await
        .map_err(|error| error.to_string())?;
    let plan_inputs = item_ids
        .iter()
        .filter_map(|item_id| {
            metadata_by_item
                .get(item_id)
                .map(|(_, current)| (item_id.as_str(), current))
        })
        .collect::<Vec<_>>();
    let plans_by_item = selection
        .local_metadata_completeness_plans(&plan_inputs)
        .await
        .map_err(|error| error.to_string())?;
    let scraper_check_item_ids =
        if auto_match_policy.allows_scraper_lookup() && metadata_reidentify.is_some() {
            item_ids
                .iter()
                .filter(|item_id| {
                    plans_by_item
                        .get(*item_id)
                        .and_then(Option::as_ref)
                        .is_some_and(|plan| plan.has_requestable_capability)
                })
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
    let (scraper_availability_by_item, scraper_availability_error) =
        if let Some(metadata_reidentify) = metadata_reidentify
            && !scraper_check_item_ids.is_empty()
        {
            match metadata_reidentify
                .has_selected_scrapers_for_items(&scraper_check_item_ids)
                .await
            {
                Ok(available) => (available, None),
                Err(error) => (HashMap::new(), Some(error.to_string())),
            }
        } else {
            (HashMap::new(), None)
        };
    let mut checks_by_library: BTreeMap<String, Vec<PendingLocalMetadataCompletenessCheck>> =
        BTreeMap::new();
    for item_id in item_ids {
        let Some((library_id, _current)) = metadata_by_item.get(item_id) else {
            continue;
        };
        let Some(plan) = plans_by_item.get(item_id).and_then(Option::as_ref) else {
            continue;
        };
        let eligible_for_fill_missing = if plan.has_requestable_capability
            && auto_match_policy.allows_scraper_lookup()
        {
            if metadata_reidentify.is_some() {
                let availability = scraper_availability_by_item.get(item_id);
                let preflight_result = if let Some(error) = scraper_availability_error.as_deref() {
                    tracing::warn!(
                        item_id = %item_id,
                        error,
                        "local metadata can confirm missing capabilities but scraper availability could not be checked"
                    );
                    Err(())
                } else {
                    match availability {
                        Some(Ok(available)) => Ok(Some(*available)),
                        None => Ok(None),
                        Some(Err(error)) => {
                            tracing::warn!(
                                item_id = %item_id,
                                %error,
                                "local metadata can confirm missing capabilities but scraper availability could not be checked"
                            );
                            Err(())
                        }
                    }
                };
                let preflight = ScraperAvailabilityPreflight::from_check_result(preflight_result);
                // The metadata worker can persist SCRAPER_UNAVAILABLE and apply its bounded retry.
                preflight.allows_fill_missing()
            } else {
                false
            }
        } else {
            false
        };
        let checks = checks_by_library.entry(library_id.clone()).or_default();
        checks.extend(plan.capabilities.iter().map(|(capability, is_missing)| {
            (
                item_id.clone(),
                capability.clone(),
                plan.input_fingerprint.clone(),
                *is_missing,
                eligible_for_fill_missing,
            )
        }));
    }

    let checked_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        });
    let mut scheduled_job_ids = Vec::new();
    for (library_id, checks) in checks_by_library {
        for check_batch in checks.chunks(SCAN_LOCAL_METADATA_COMPLETENESS_CHECK_BATCH_SIZE) {
            let results = check_batch
                .iter()
                .map(|(item_id, capability, input_fingerprint, is_missing, _)| {
                    NewItemMetadataCompletenessResult {
                        item_id,
                        capability,
                        input_fingerprint,
                        is_missing: *is_missing,
                        checked_at,
                    }
                })
                .collect::<Vec<_>>();
            let mut fill_missing_candidate_item_ids = check_batch
                .iter()
                .filter(|(_, _, _, is_missing, eligible)| *is_missing && *eligible)
                .map(|(item_id, _, _, _, _)| item_id.clone())
                .collect::<Vec<_>>();
            fill_missing_candidate_item_ids.sort_unstable();
            fill_missing_candidate_item_ids.dedup();
            let completion = database
                .complete_local_metadata_completeness_batch_with_policy(
                    &library_id,
                    &results,
                    &fill_missing_candidate_item_ids,
                    auto_match_policy,
                )
                .await
                .map_err(|error| error.to_string())?;
            scheduled_job_ids.extend(
                completion
                    .scheduled_job_ids
                    .into_iter()
                    .map(|job_id| (library_id.clone(), job_id)),
            );
        }
    }
    if let Some(metadata_reidentify) = metadata_reidentify.cloned()
        && !scheduled_job_ids.is_empty()
    {
        let mut completions = Vec::with_capacity(scheduled_job_ids.len());
        for (library_id, job_id) in scheduled_job_ids {
            match metadata_reidentify
                .enqueue_fill_missing_job_id(&job_id)
                .await
            {
                Ok(Some(completion)) => completions.push(completion),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        library_id,
                        job_id,
                        %error,
                        "local metadata completeness job could not be dispatched"
                    );
                }
            }
        }
        if !completions.is_empty() {
            let user_events = user_events.clone();
            tokio::spawn(async move {
                for completion in completions {
                    let _ = completion.await;
                    user_events.publish_home_coalesced().await;
                }
            });
        }
    }
    Ok(())
}

impl ScanJobService {
    pub fn new(database: Database) -> Self {
        let log_store = database.log_store();
        Self {
            scanner: LibraryScanner::new(database.clone()),
            database,
            admin_events: AdminEventHub::new(),
            user_events: UserEventHub::new(),
            scan_lock: Arc::new(Semaphore::new(1)),
            full_scan_queue: Arc::new(Semaphore::new(1)),
            library_covers: None,
            strm_probe: None,
            people: None,
            local_nfo: None,
            metadata_selection: None,
            metadata_reidentify: None,
            home: None,
            webhooks: None,
            log_store,
            resources: ResourceMetrics::new(),
            default_scan_concurrency: usize::try_from(
                scan_concurrency_from_env().unwrap_or(DEFAULT_SCAN_CONCURRENCY),
            )
            .unwrap_or(1),
            scan_concurrency_override: scan_concurrency_override_from_env()
                .ok()
                .flatten()
                .and_then(|value| usize::try_from(value).ok()),
            cancellation_flags: Arc::new(Mutex::new(HashMap::new())),
            scan_run_registry: Arc::new(Mutex::new(ScanRunRegistry::default())),
            metadata_notifications: Arc::new(Mutex::new(HashMap::new())),
            local_metadata_image_notifications: Arc::new(Mutex::new(HashMap::new())),
            local_metadata_outbox_worker_started: Arc::new(AtomicBool::new(false)),
            local_metadata_outbox_notify: Arc::new(Notify::new()),
            lite_manifest_discovery: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn with_scan_lock(mut self, scan_lock: Arc<Semaphore>) -> Self {
        self.scan_lock = scan_lock;
        self
    }

    pub fn with_admin_events(mut self, admin_events: AdminEventHub) -> Self {
        self.admin_events = admin_events;
        self
    }

    pub fn with_user_events(mut self, user_events: UserEventHub) -> Self {
        self.user_events = user_events;
        self
    }

    pub fn with_log_store(mut self, log_store: LogStore) -> Self {
        self.log_store = log_store;
        self
    }

    pub fn with_library_covers(mut self, library_covers: LibraryCoverService) -> Self {
        self.library_covers = Some(library_covers);
        self
    }

    pub fn with_strm_probe(mut self, strm_probe: StrmProbeService) -> Self {
        self.strm_probe = Some(strm_probe);
        self
    }

    pub fn with_people(mut self, people: PeopleService) -> Self {
        self.people = Some(people);
        self
    }

    pub fn with_nfo_store(mut self, local_nfo: LocalNfoMetadataStore) -> Self {
        self.local_nfo = Some(local_nfo);
        self
    }

    pub(crate) fn with_metadata_selection(
        mut self,
        metadata_selection: MetadataSelectionService,
    ) -> Self {
        self.metadata_selection = Some(metadata_selection);
        self
    }

    pub(crate) fn with_metadata_reidentify(
        mut self,
        metadata_reidentify: MetadataReidentifyService,
    ) -> Self {
        self.metadata_reidentify = Some(metadata_reidentify);
        self
    }

    pub(crate) fn with_home(mut self, home: HomeService) -> Self {
        self.home = Some(home);
        self
    }

    pub fn with_webhooks(mut self, webhooks: WebhookService) -> Self {
        self.webhooks = Some(webhooks);
        self
    }

    pub fn with_movie_nfo_store(self, local_nfo: LocalNfoMetadataStore) -> Self {
        self.with_nfo_store(local_nfo)
    }

    pub fn with_resource_metrics(mut self, resources: ResourceMetrics) -> Self {
        self.resources = resources;
        self
    }

    fn cancellation_flag(&self, job_id: &str) -> Arc<AtomicBool> {
        let mut flags = match self.cancellation_flags.lock() {
            Ok(flags) => flags,
            Err(poisoned) => poisoned.into_inner(),
        };
        flags
            .entry(job_id.to_owned())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    }

    fn clear_cancellation_flag(&self, job_id: &str) {
        let mut flags = match self.cancellation_flags.lock() {
            Ok(flags) => flags,
            Err(poisoned) => poisoned.into_inner(),
        };
        flags.remove(job_id);
    }

    fn track_scan_job_run(&self, job_id: &str, library_id: &str) -> Option<ScanJobRunGuard> {
        let mut registry = match self.scan_run_registry.lock() {
            Ok(registry) => registry,
            Err(poisoned) => poisoned.into_inner(),
        };
        if registry.deleting_libraries.contains_key(library_id) {
            return None;
        }
        let active_run = registry
            .active_runs
            .entry(job_id.to_owned())
            .or_insert_with(|| ActiveScanJobRun {
                library_id: library_id.to_owned(),
                run_count: 0,
            });
        active_run.run_count += 1;
        Some(ScanJobRunGuard {
            registry: Arc::clone(&self.scan_run_registry),
            job_id: job_id.to_owned(),
        })
    }

    fn scan_job_run_is_active(&self, job_id: &str) -> bool {
        let registry = match self.scan_run_registry.lock() {
            Ok(registry) => registry,
            Err(poisoned) => poisoned.into_inner(),
        };
        registry.active_runs.contains_key(job_id)
    }

    async fn wait_for_scan_job_runs_to_finish(&self, job_ids: &[String]) {
        loop {
            let run_finished = {
                let registry = match self.scan_run_registry.lock() {
                    Ok(registry) => registry,
                    Err(poisoned) => poisoned.into_inner(),
                };
                Arc::clone(&registry.run_finished)
            };
            let notified = run_finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let has_active_runs = {
                let registry = match self.scan_run_registry.lock() {
                    Ok(registry) => registry,
                    Err(poisoned) => poisoned.into_inner(),
                };
                job_ids
                    .iter()
                    .any(|job_id| registry.active_runs.contains_key(job_id))
            };
            if !has_active_runs {
                return;
            }
            notified.await;
        }
    }

    async fn ensure_lite_manifest_discovery_session(
        &self,
        manifest_id: &str,
    ) -> Result<(), ScanJobError> {
        {
            let sessions = match self.lite_manifest_discovery.lock() {
                Ok(sessions) => sessions,
                Err(poisoned) => poisoned.into_inner(),
            };
            if sessions.contains_key(manifest_id) {
                return Ok(());
            }
        }

        let roots = self
            .database
            .list_scan_manifest_root_discovery_baselines(manifest_id)
            .await?;
        let mut session = LiteManifestDiscoverySession::default();
        for (root_id, state, has_filesystem_entries) in roots {
            if matches!(state.as_str(), "COMPLETE" | "UNAVAILABLE") {
                continue;
            }
            session
                .directories
                .push_back((root_id, String::new(), has_filesystem_entries));
        }
        let mut sessions = match self.lite_manifest_discovery.lock() {
            Ok(sessions) => sessions,
            Err(poisoned) => poisoned.into_inner(),
        };
        sessions.entry(manifest_id.to_owned()).or_insert(session);
        Ok(())
    }

    fn pop_lite_manifest_directories(
        &self,
        manifest_id: &str,
        limit: usize,
    ) -> Vec<(String, String, bool)> {
        let mut sessions = match self.lite_manifest_discovery.lock() {
            Ok(sessions) => sessions,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(session) = sessions.get_mut(manifest_id) else {
            return Vec::new();
        };
        let mut directories = Vec::with_capacity(limit);
        for _ in 0..limit {
            let Some(directory) = session.directories.pop_front() else {
                break;
            };
            directories.push(directory);
        }
        directories
    }

    fn push_lite_manifest_directories(
        &self,
        manifest_id: &str,
        root_id: &str,
        has_filesystem_entries: bool,
        directories: impl IntoIterator<Item = String>,
    ) {
        let mut sessions = match self.lite_manifest_discovery.lock() {
            Ok(sessions) => sessions,
            Err(poisoned) => poisoned.into_inner(),
        };
        let Some(session) = sessions.get_mut(manifest_id) else {
            return;
        };
        session.directories.extend(
            directories
                .into_iter()
                .map(|directory| (root_id.to_owned(), directory, has_filesystem_entries)),
        );
    }

    fn remove_lite_manifest_root(&self, manifest_id: &str, root_id: &str) {
        let mut sessions = match self.lite_manifest_discovery.lock() {
            Ok(sessions) => sessions,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(session) = sessions.get_mut(manifest_id) {
            session
                .directories
                .retain(|(queued_root_id, _, _)| queued_root_id != root_id);
        }
    }

    fn clear_lite_manifest_discovery_session(&self, manifest_id: &str) {
        let mut sessions = match self.lite_manifest_discovery.lock() {
            Ok(sessions) => sessions,
            Err(poisoned) => poisoned.into_inner(),
        };
        sessions.remove(manifest_id);
    }

    fn lite_manifest_has_pending_directories(&self, manifest_id: &str) -> bool {
        let sessions = match self.lite_manifest_discovery.lock() {
            Ok(sessions) => sessions,
            Err(poisoned) => poisoned.into_inner(),
        };
        sessions
            .get(manifest_id)
            .is_some_and(|session| !session.directories.is_empty())
    }

    async fn flush_home_after_scan_terminal(&self) {
        if let Some(home) = &self.home {
            if home.flush_scan_invalidation().await {
                self.user_events.publish_home_coalesced().await;
            }
        }
    }

    fn cancellation_requested_in_memory(&self, job_id: &str) -> bool {
        let flags = match self.cancellation_flags.lock() {
            Ok(flags) => flags,
            Err(poisoned) => poisoned.into_inner(),
        };
        flags
            .get(job_id)
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    }

    pub async fn prepare_library_deletion(
        &self,
        library_id: LibraryId,
    ) -> Result<LibraryDeletionGuard, ScanJobError> {
        let library_id = library_id.to_string();
        let (deletion_guard, active_job_ids) = {
            let mut registry = match self.scan_run_registry.lock() {
                Ok(registry) => registry,
                Err(poisoned) => poisoned.into_inner(),
            };
            *registry
                .deleting_libraries
                .entry(library_id.clone())
                .or_default() += 1;
            let active_job_ids = registry
                .active_runs
                .iter()
                .filter(|(_, active_run)| active_run.library_id == library_id)
                .map(|(job_id, _)| job_id.clone())
                .collect::<Vec<_>>();
            (
                LibraryDeletionGuard {
                    registry: Arc::clone(&self.scan_run_registry),
                    library_id: library_id.clone(),
                },
                active_job_ids,
            )
        };
        let jobs = self
            .database
            .list_scan_jobs_for_library_deletion(&library_id)
            .await?;
        let mut cancellation_job_ids = active_job_ids.into_iter().collect::<HashSet<_>>();
        cancellation_job_ids.extend(jobs.into_iter().map(|job| job.id));
        let active_job_ids = cancellation_job_ids
            .iter()
            .filter(|job_id| self.scan_job_run_is_active(job_id))
            .cloned()
            .collect::<Vec<_>>();
        for job_id in cancellation_job_ids {
            self.cancellation_flag(&job_id)
                .store(true, Ordering::Release);
            self.database.request_scan_job_cancel(&job_id).await?;
            if !self.scan_job_run_is_active(&job_id) {
                self.cancel_running_job(&job_id).await?;
            }
        }
        if tokio::time::timeout(
            LIBRARY_DELETION_SCAN_CANCEL_TIMEOUT,
            self.wait_for_scan_job_runs_to_finish(&active_job_ids),
        )
        .await
        .is_err()
        {
            return Err(ScanJobError::LibraryDeletionScanTimeout);
        }
        Ok(deletion_guard)
    }

    async fn cancel_running_job(&self, job_id: &str) -> Result<ScanBatchReport, ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            self.clear_cancellation_flag(job_id);
            self.flush_home_after_scan_terminal().await;
            return Ok(ScanBatchReport {
                status: "CANCELLED".to_owned(),
                processed: 0,
                created_items: 0,
                completed: true,
            });
        };
        self.database
            .cancel_scan_local_metadata_batches(job_id)
            .await?;
        self.database
            .finish_scan_manifest(job_id, "CANCELLED")
            .await?;
        if let Some(manifest) = self.database.get_scan_manifest_by_job(job_id).await? {
            self.clear_lite_manifest_discovery_session(&manifest.id);
        }
        self.database
            .clear_reconciliation_scan_entries(job_id)
            .await?;
        self.database.clear_scan_job_paths(job_id).await?;
        self.database
            .finish_scan_job(job_id, "CANCELLED", None)
            .await?;
        if job.scan_phase != "POSTPROCESSING"
            && let Some(home) = &self.home
        {
            home.invalidate_scan_batch().await;
        }
        self.flush_home_after_scan_terminal().await;
        self.record_event(job_id, "INFO", "JOB_CANCELLED", "任务已取消", "{}")
            .await;
        self.clear_cancellation_flag(job_id);
        Ok(ScanBatchReport {
            status: "CANCELLED".to_owned(),
            processed: 0,
            created_items: 0,
            completed: true,
        })
    }

    async fn cancellation_requested(
        &self,
        job_id: &str,
        job_cancel_requested: bool,
        flag: &AtomicBool,
    ) -> Result<bool, ScanJobError> {
        if job_cancel_requested || flag.load(Ordering::Acquire) {
            flag.store(true, Ordering::Release);
            return Ok(true);
        }
        if self.database.find_scan_job(job_id).await?.is_none() {
            flag.store(true, Ordering::Release);
            return Ok(true);
        }
        if self.database.scan_job_cancel_requested(job_id).await? {
            flag.store(true, Ordering::Release);
            return Ok(true);
        }
        Ok(false)
    }

    async fn update_activity(
        &self,
        job_id: &str,
        current_item: Option<&str>,
        scan_phase: &str,
    ) -> Result<(), ScanJobError> {
        let current_item = current_item.and_then(safe_scan_activity_label);
        self.database
            .update_scan_job_activity(job_id, current_item.as_deref(), scan_phase)
            .await?;
        self.admin_events.publish_jobs_progress();
        Ok(())
    }

    pub async fn enqueue_incremental_changes(
        &self,
        library_id: LibraryId,
        changes: Vec<IncrementalScanChange>,
    ) -> Result<ScanJob, ScanJobError> {
        let library_id_text = library_id.to_string();
        let Some(library) = self.database.find_library(&library_id_text).await? else {
            return Err(ScanJobError::LibraryNotFound);
        };
        if !library.is_enabled {
            return Err(ScanJobError::LibraryNotFound);
        }
        let roots = self.database.list_library_roots(&library_id_text).await?;
        let mut valid_changes = Vec::new();
        for change in changes {
            let Some(root) = roots.iter().find(|root| root.id == change.root_id) else {
                continue;
            };
            let relative_path = normalize_incremental_path(&change.relative_path)?;
            if !relative_path.is_empty() {
                valid_changes.push((root.id.clone(), relative_path, change.kind));
            }
        }
        if valid_changes.is_empty() {
            return Err(ScanJobError::NoChanges);
        }
        let job = self
            .get_or_create_incremental_scan_job_reusing_active(
                &library_id_text,
                library.realtime_metadata_auto_match_enabled,
            )
            .await?;
        self.cancellation_flag(&job.id);
        let paths = valid_changes
            .iter()
            .map(|(root_id, relative_path, kind)| {
                (
                    root_id.as_str(),
                    relative_path.as_str(),
                    change_kind_name(*kind),
                )
            })
            .collect::<Vec<_>>();
        self.database
            .enqueue_incremental_scan_paths(&job.id, &paths)
            .await?;
        self.record_event(
            &job.id,
            "INFO",
            "PATHS_QUEUED",
            "已加入局部增量扫描路径",
            "{}",
        )
        .await;
        self.get_job(&job.id).await
    }

    /// Re-runs local metadata indexing (NFO and images) for the media files below exact
    /// directories of a library root, without walking the rest of the root.
    ///
    /// This fills the gap between a path scan, which only looks at files whose fingerprint
    /// changed, and a library backfill, which walks every file of the root: after an external
    /// tool writes or repairs sidecars (a scraper publishing a poster, a rebuilt item that lost
    /// its images) nothing about the video files changes, so only this refreshes them.
    /// Work runs in the background; the returned counts describe what was queued.
    pub async fn start_local_metadata_refresh(
        &self,
        library_id: LibraryId,
        library_root_id: Option<&str>,
        directories: &[String],
    ) -> Result<LocalMetadataRefreshAccepted, ScanJobError> {
        const MAX_DIRECTORIES: usize = 2_000;
        const MAX_ENTRIES: i64 = 20_000;
        if directories.is_empty() {
            return Err(ScanJobError::NoChanges);
        }
        if directories.len() > MAX_DIRECTORIES {
            return Err(ScanJobError::Scanner(ScannerError::InvalidRelativePath(
                format!("at most {MAX_DIRECTORIES} directories per request"),
            )));
        }
        let library_id = library_id.to_string();
        let Some(library) = self.database.find_library(&library_id).await? else {
            return Err(ScanJobError::LibraryNotFound);
        };
        if !library.is_enabled {
            return Err(ScanJobError::LibraryNotFound);
        }
        let roots = self.database.list_library_roots(&library_id).await?;
        let root = match library_root_id {
            Some(root_id) => roots
                .into_iter()
                .find(|root| root.id == root_id)
                .ok_or_else(|| {
                    ScanJobError::Scanner(ScannerError::InvalidRootId(root_id.to_owned()))
                })?,
            None => {
                let mut roots = roots.into_iter();
                let Some(root) = roots.next() else {
                    return Err(ScanJobError::Scanner(ScannerError::InvalidRootId(
                        "library has no configured roots".to_owned(),
                    )));
                };
                if roots.next().is_some() {
                    return Err(ScanJobError::Scanner(ScannerError::InvalidRootId(
                        "library has multiple roots; rootId is required".to_owned(),
                    )));
                }
                root
            }
        };
        let mut normalized = Vec::with_capacity(directories.len());
        for directory in directories {
            let directory = normalize_incremental_path(directory)?;
            let directory = directory.trim_end_matches('/').to_owned();
            if !directory.is_empty() && !normalized.contains(&directory) {
                normalized.push(directory);
            }
        }
        let entry_ids = self
            .database
            .list_media_source_entry_ids_under_directories(&root.id, &normalized, MAX_ENTRIES)
            .await?;
        let accepted = LocalMetadataRefreshAccepted {
            directories: normalized.len(),
            entries: entry_ids.len(),
        };
        if entry_ids.is_empty() {
            return Ok(accepted);
        }
        let service = self.clone();
        tokio::spawn(async move {
            service.run_local_metadata_refresh(entry_ids).await;
        });
        Ok(accepted)
    }

    async fn run_local_metadata_refresh(&self, entry_ids: Vec<String>) {
        // One refresh at a time: it is cheap per entry, but a burst of notifications must not
        // compete with scans for the database.
        static REFRESH_LOCK: OnceLock<Semaphore> = OnceLock::new();
        let Ok(_permit) = REFRESH_LOCK
            .get_or_init(|| Semaphore::new(1))
            .acquire()
            .await
        else {
            return;
        };
        let enricher = MetadataEnricher::new(self.database.clone())
            .with_resource_metrics(self.resources.clone());
        let enricher = match self.local_nfo.clone() {
            Some(local_nfo) => enricher.with_nfo_store(local_nfo),
            None => enricher,
        };
        let mut failed_items = 0_usize;
        for chunk in entry_ids.chunks(64) {
            let source_snapshot = match enricher.index_scan_local_metadata_batch_images(chunk).await
            {
                Ok(batch) => batch.sources,
                Err(error) => {
                    tracing::warn!(%error, "local metadata refresh could not index images");
                    failed_items += 1;
                    continue;
                }
            };
            match enricher
                .enrich_scan_local_metadata_batch_nfo(source_snapshot, &[])
                .await
            {
                Ok(ScanLocalMetadataNfoBatch {
                    report,
                    source_identities,
                }) => {
                    failed_items += report.failed_item_ids.len();
                    if let Err(error) = complete_local_metadata_completeness(
                        &self.database,
                        self.metadata_selection.as_ref(),
                        self.metadata_reidentify.as_ref(),
                        LocalMetadataCompletenessTrigger::LibrarySetting,
                        &source_identities,
                        &report.non_retryable_failed_item_ids,
                        &self.user_events,
                    )
                    .await
                    {
                        tracing::warn!(%error, "local metadata refresh could not update completeness");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "local metadata refresh could not read NFOs");
                    failed_items += 1;
                }
            }
        }
        tracing::info!(
            entries = entry_ids.len(),
            failed_items,
            "local metadata refresh finished"
        );
        if let Some(home) = &self.home {
            home.invalidate();
        }
        self.user_events.publish_home_coalesced().await;
    }

    pub async fn create_path_scan_job(
        &self,
        library_id: LibraryId,
        library_root_id: Option<&str>,
        relative_path: &str,
    ) -> Result<ScanJob, ScanJobError> {
        let library_id = library_id.to_string();
        let Some(library) = self.database.find_library(&library_id).await? else {
            return Err(ScanJobError::LibraryNotFound);
        };
        if !library.is_enabled {
            return Err(ScanJobError::LibraryNotFound);
        }
        let roots = self.database.list_library_roots(&library_id).await?;
        let root = match library_root_id {
            Some(root_id) => roots
                .into_iter()
                .find(|root| root.id == root_id)
                .ok_or_else(|| {
                    ScanJobError::Scanner(ScannerError::InvalidRootId(root_id.to_owned()))
                })?,
            None => {
                let mut roots = roots.into_iter();
                let Some(root) = roots.next() else {
                    return Err(ScanJobError::Scanner(ScannerError::InvalidRootId(
                        "library has no configured roots".to_owned(),
                    )));
                };
                if roots.next().is_some() {
                    return Err(ScanJobError::Scanner(ScannerError::InvalidRootId(
                        "library has multiple roots; rootId is required".to_owned(),
                    )));
                }
                root
            }
        };
        self.create_incremental_path_scan_job(&library_id, &root.id, relative_path)
            .await
    }

    pub async fn create_library_root_scan_job(
        &self,
        library_id: LibraryId,
    ) -> Result<ScanJob, ScanJobError> {
        let library_id = library_id.to_string();
        let Some(library) = self.database.find_library(&library_id).await? else {
            return Err(ScanJobError::LibraryNotFound);
        };
        if !library.is_enabled {
            return Err(ScanJobError::LibraryNotFound);
        }
        let roots = self.database.list_library_roots(&library_id).await?;
        if roots.is_empty() {
            return Err(ScanJobError::Scanner(ScannerError::InvalidRootId(
                "library has no configured roots".to_owned(),
            )));
        }
        let job = self
            .get_or_create_incremental_scan_job_reusing_active(&library_id, false)
            .await?;
        self.cancellation_flag(&job.id);
        let paths = roots
            .iter()
            .map(|root| (root.id.as_str(), ".", "MODIFY"))
            .collect::<Vec<_>>();
        self.database
            .enqueue_incremental_scan_paths(&job.id, &paths)
            .await?;
        self.record_event(
            &job.id,
            "WARN",
            "LIBRARY_ROOT_REFRESH_QUEUED",
            "未解析到具体文件夹，已加入媒体库根目录局部扫描",
            "{}",
        )
        .await;
        self.get_job(&job.id).await
    }

    pub async fn create_folder_scan_job(&self, item_id: &str) -> Result<ScanJob, ScanJobError> {
        let Some(source) = self.database.find_folder_scan_path(item_id).await? else {
            return Err(ScanJobError::ItemNotFound);
        };
        self.create_incremental_path_scan_job(
            &source.library_id,
            &source.library_root_id,
            &source.relative_path,
        )
        .await
    }

    async fn create_incremental_path_scan_job(
        &self,
        library_id: &str,
        library_root_id: &str,
        relative_path: &str,
    ) -> Result<ScanJob, ScanJobError> {
        let Some(library) = self.database.find_library(library_id).await? else {
            return Err(ScanJobError::LibraryNotFound);
        };
        if !library.is_enabled {
            return Err(ScanJobError::LibraryNotFound);
        }
        let roots = self.database.list_library_roots(library_id).await?;
        if !roots.iter().any(|root| root.id == library_root_id) {
            return Err(ScanJobError::Scanner(ScannerError::InvalidRootId(
                library_root_id.to_owned(),
            )));
        }
        let relative_path = normalize_incremental_path(relative_path)?;
        let job = self
            .get_or_create_incremental_scan_job_reusing_active(library_id, false)
            .await?;
        self.cancellation_flag(&job.id);
        self.database
            .enqueue_incremental_scan_path(&job.id, library_root_id, &relative_path, "MODIFY")
            .await?;
        self.record_event(
            &job.id,
            "INFO",
            "PATHS_QUEUED",
            "已加入指定路径局部增量扫描",
            "{}",
        )
        .await;
        self.get_job(&job.id).await
    }

    pub async fn create_item_folder_scan_job(
        &self,
        item_id: &str,
    ) -> Result<ScanJob, ScanJobError> {
        let Some(source) = self.database.find_item_scan_source_path(item_id).await? else {
            return Err(ScanJobError::ItemNotFound);
        };
        let Some(library) = self.database.find_library(&source.library_id).await? else {
            return Err(ScanJobError::LibraryNotFound);
        };
        if !library.is_enabled {
            return Err(ScanJobError::LibraryNotFound);
        }
        let folder = media_source_folder(&source.relative_path)?;
        let job = self
            .get_or_create_incremental_scan_job(&source.library_id, false)
            .await?;
        self.cancellation_flag(&job.id);
        self.database
            .enqueue_incremental_scan_path(&job.id, &source.library_root_id, &folder, "MODIFY")
            .await?;
        self.record_event(
            &job.id,
            "INFO",
            "PATHS_QUEUED",
            "已加入媒体所在文件夹扫描路径",
            "{}",
        )
        .await;
        self.get_job(&job.id).await
    }

    async fn get_or_create_incremental_scan_job(
        &self,
        library_id: &str,
        auto_metadata_match: bool,
    ) -> Result<StoredScanJob, ScanJobError> {
        self.get_or_create_incremental_scan_job_with_policy(library_id, auto_metadata_match, false)
            .await
    }

    async fn get_or_create_incremental_scan_job_reusing_active(
        &self,
        library_id: &str,
        auto_metadata_match: bool,
    ) -> Result<StoredScanJob, ScanJobError> {
        self.get_or_create_incremental_scan_job_with_policy(library_id, auto_metadata_match, true)
            .await
    }

    async fn get_or_create_incremental_scan_job_with_policy(
        &self,
        library_id: &str,
        auto_metadata_match: bool,
        reuse_active: bool,
    ) -> Result<StoredScanJob, ScanJobError> {
        if let Some(active) = self
            .database
            .find_active_scan_job(library_id, "INCREMENTAL_SCAN")
            .await?
        {
            if !reuse_active {
                return Err(ScanJobError::AlreadyActive(active.id));
            }
            if auto_metadata_match && !active.auto_metadata_match {
                self.database
                    .enable_scan_job_auto_metadata_match(&active.id)
                    .await?;
            }
            return Ok(active);
        }
        let id = Uuid::now_v7().to_string();
        let generation = Uuid::now_v7().to_string();
        if let Err(error) = self
            .database
            .create_scan_job(
                &id,
                library_id,
                "INCREMENTAL_SCAN",
                &generation,
                0,
                auto_metadata_match,
            )
            .await
        {
            if error.is_unique_violation()
                && let Some(active) = self
                    .database
                    .find_active_scan_job(library_id, "INCREMENTAL_SCAN")
                    .await?
            {
                return if reuse_active {
                    Ok(active)
                } else {
                    Err(ScanJobError::AlreadyActive(active.id))
                };
            }
            return Err(error.into());
        }
        self.database
            .find_scan_job(&id)
            .await?
            .ok_or(ScanJobError::JobNotFound)
    }

    pub async fn create_movie_scan_job(
        &self,
        library_id: LibraryId,
    ) -> Result<ScanJob, ScanJobError> {
        self.create_movie_scan_job_with_metadata(library_id, false)
            .await
    }

    pub async fn create_movie_scan_job_with_metadata(
        &self,
        library_id: LibraryId,
        auto_metadata_match: bool,
    ) -> Result<ScanJob, ScanJobError> {
        self.create_movie_scan_job_with_metadata_and_legacy_retry(
            library_id,
            auto_metadata_match,
            None,
        )
        .await
    }

    async fn create_movie_scan_job_with_metadata_and_legacy_retry(
        &self,
        library_id: LibraryId,
        auto_metadata_match: bool,
        legacy_retry_job_id: Option<&str>,
    ) -> Result<ScanJob, ScanJobError> {
        let library_id_text = library_id.to_string();
        let Some(library) = self.database.find_library(&library_id_text).await? else {
            return Err(ScanJobError::LibraryNotFound);
        };
        if !library.is_enabled {
            return Err(ScanJobError::LibraryNotFound);
        }
        if let Some(active) = self
            .database
            .find_active_scan_job_for_library(&library_id_text)
            .await?
        {
            return Err(ScanJobError::AlreadyActive(active.id));
        }
        let roots = self.database.list_library_roots(&library_id_text).await?;
        let id = Uuid::now_v7().to_string();
        let generation = Uuid::now_v7().to_string();
        let manifest_id = Uuid::now_v7().to_string();
        let manifest_roots = roots
            .iter()
            .map(|root| NewScanManifestRoot {
                library_root_id: &root.id,
            })
            .collect::<Vec<_>>();
        let manifest = NewScanManifest {
            id: &manifest_id,
            job_id: &id,
            library_id: &library_id_text,
            roots: &manifest_roots,
        };
        if let Err(error) = self
            .database
            .create_full_scan_manifest_job(
                &id,
                &generation,
                auto_metadata_match,
                &manifest,
                legacy_retry_job_id,
            )
            .await
        {
            if error.is_unique_violation()
                && let Some(active) = self
                    .database
                    .find_active_scan_job_for_library(&library_id_text)
                    .await?
            {
                return Err(ScanJobError::AlreadyActive(active.id));
            }
            return Err(error.into());
        }
        self.cancellation_flag(&id);
        self.record_event(&id, "INFO", "JOB_CREATED", "任务已创建", "{}")
            .await;
        self.get_job(&id).await
    }

    pub async fn run_batch(
        &self,
        job_id: &str,
        batch_size: usize,
    ) -> Result<ScanBatchReport, ScanJobError> {
        if batch_size == 0 {
            return Err(ScanJobError::InvalidBatchSize);
        }
        let _scan_permit = self.acquire_scan_lock_for_job(job_id).await?;
        let report = match self
            .run_batch_with_failure_handling(job_id, batch_size, false)
            .await
        {
            Ok(report) => report,
            Err(error) => {
                self.flush_home_after_scan_terminal().await;
                return Err(error);
            }
        };
        if report.status == "COMPLETED" {
            self.flush_home_after_scan_terminal().await;
        } else if report.processed > 0
            && let Some(home) = &self.home
        {
            home.invalidate_scan_batch().await;
        }
        Ok(report)
    }

    async fn run_batch_with_failure_handling(
        &self,
        job_id: &str,
        batch_size: usize,
        stream_files_during_discovery: bool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let batch_size = if batch_size > MAX_SCAN_JOB_BATCH_SIZE {
            BACKGROUND_SCAN_BATCH_SIZE
        } else {
            batch_size
        };
        let mut retry_attempt = 0;
        loop {
            match self
                .run_batch_unlocked(job_id, batch_size, stream_files_during_discovery)
                .await
            {
                Ok(report) => return Ok(report),
                Err(error)
                    if error.is_retryable_sqlite_lock_contention()
                        && retry_attempt < SQLITE_SCAN_LOCK_RETRY_DELAYS_MS.len() =>
                {
                    let retry_delay_ms = SQLITE_SCAN_LOCK_RETRY_DELAYS_MS[retry_attempt];
                    retry_attempt += 1;
                    tracing::warn!(
                        job_id,
                        retry_attempt,
                        retry_delay_ms,
                        error_code = error.code(),
                        "SQLite write contention during scan; retrying the current batch"
                    );
                    tokio::time::sleep(Duration::from_millis(retry_delay_ms)).await;
                }
                Err(error) => {
                    self.fail_unhandled_scan_job(job_id, &error).await?;
                    return Err(error);
                }
            }
        }
    }

    async fn run_batch_unlocked(
        &self,
        job_id: &str,
        batch_size: usize,
        stream_files_during_discovery: bool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            if self.cancellation_requested_in_memory(job_id) {
                self.clear_cancellation_flag(job_id);
                return Ok(ScanBatchReport {
                    status: "CANCELLED".to_owned(),
                    processed: 0,
                    created_items: 0,
                    completed: true,
                });
            }
            return Err(ScanJobError::JobNotFound);
        };
        let cancellation = self.cancellation_flag(job_id);
        if job.cancel_requested {
            cancellation.store(true, Ordering::Release);
        }
        if job.scan_phase == "POSTPROCESSING" {
            if self
                .cancellation_requested(job_id, job.cancel_requested, &cancellation)
                .await?
            {
                return self.cancel_running_job(job_id).await;
            }
            return Ok(ScanBatchReport {
                status: "COMPLETED".to_owned(),
                processed: 0,
                created_items: 0,
                completed: true,
            });
        }
        if job.job_type == "INCREMENTAL_SCAN" {
            return self
                .run_incremental_batch(job_id, batch_size, &cancellation)
                .await;
        }
        if matches!(job.status.as_str(), "COMPLETED" | "CANCELLED" | "FAILED") {
            self.clear_cancellation_flag(job_id);
            return Ok(ScanBatchReport {
                status: job.status,
                processed: 0,
                created_items: 0,
                completed: true,
            });
        }
        if job.status == "PENDING" {
            if !self.database.claim_scan_job(job_id).await? {
                return Err(ScanJobError::AlreadyActive(job_id.to_owned()));
            }
            self.record_event(job_id, "INFO", "JOB_STARTED", "任务开始执行", "{}")
                .await;
        }
        if self
            .cancellation_requested(job_id, job.cancel_requested, &cancellation)
            .await?
        {
            return self.cancel_running_job(job_id).await;
        }

        if let Some(manifest) = self.database.get_scan_manifest_by_job(&job.id).await? {
            let lite_mode = is_lite_manifest_discovery(
                manifest.workflow_version,
                manifest.discovery_format_version,
                &manifest.discovery_mode,
            );
            return match manifest.state.as_str() {
                "DISCOVERING" => {
                    self.run_scan_manifest_discovery_batch(
                        &job,
                        &manifest.id,
                        batch_size,
                        &cancellation,
                        matches!(manifest.workflow_version, 2 | 3),
                        lite_mode,
                    )
                    .await
                }
                "READY_TO_DIFF" => {
                    self.run_scan_manifest_diff_batch(
                        &job,
                        &manifest.id,
                        &cancellation,
                        matches!(manifest.workflow_version, 2 | 3),
                        manifest.discovery_format_version,
                        lite_mode,
                    )
                    .await
                }
                "APPLYING" => {
                    let apply_batch_size = if stream_files_during_discovery
                        && batch_size == BACKGROUND_SCAN_BATCH_SIZE
                    {
                        MANIFEST_APPLY_BATCH_SIZE
                    } else {
                        batch_size
                    };
                    self.run_scan_manifest_apply_batch(
                        &job,
                        &manifest.id,
                        apply_batch_size,
                        &cancellation,
                    )
                    .await
                }
                "INDEXED" => self.finish_scan_manifest_indexing(&job, &manifest.id).await,
                "POSTPROCESSING" => Ok(ScanBatchReport {
                    status: "COMPLETED".to_owned(),
                    processed: 0,
                    created_items: 0,
                    completed: true,
                }),
                _ => Err(ScanJobError::Storage(StorageError::Conflict(
                    "scan manifest is not in an executable state".to_owned(),
                ))),
            };
        }
        if !job.discovery_completed {
            return self
                .run_reconciliation_discovery_batch(
                    &job,
                    batch_size,
                    &cancellation,
                    stream_files_during_discovery,
                )
                .await;
        }
        self.run_reconciliation_file_batch(&job, batch_size, &cancellation, true)
            .await
    }

    async fn discover_scan_manifest_directory_batches(
        &self,
        context: ManifestRootDiscoveryContext<'_>,
        relative_directory: &str,
    ) -> Result<Option<ManifestDiscoveryDirectoryResult>, ScannerError> {
        let ManifestRootDiscoveryContext {
            job_id,
            manifest_id,
            root,
            cancellation,
            stream_files_during_discovery,
            library_kind,
            preparation_concurrency,
            expected_root_identity,
            ..
        } = context;
        if cancellation.load(Ordering::Acquire) {
            return Ok(None);
        }
        let relative = Path::new(relative_directory);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::CurDir
                        | Component::ParentDir
                        | Component::RootDir
                        | Component::Prefix(_)
                )
            })
        {
            return Err(ScannerError::InvalidRelativePath(
                relative_directory.to_owned(),
            ));
        }
        let root_path = PathBuf::from(&root.canonical_path);
        let open_root_path = root_path.clone();
        let open_relative_directory = relative_directory.to_owned();
        let open_started = Instant::now();
        let mut reader = tokio::task::spawn_blocking(move || {
            ManifestDirectoryReader::open(&open_root_path, &open_relative_directory)
        })
        .await
        .map_err(|source| ScannerError::Io {
            path: root_path.clone(),
            source: std::io::Error::other(source.to_string()),
        })??;
        record_manifest_scan_stage("directory_open", open_started, 1, 0, 1);
        record_manifest_scan_activity(0, 1);
        let root_observation = reader.root_observation.clone();
        let directory_observation = reader.directory_observation.clone();
        let mut result = ManifestDiscoveryDirectoryResult::default();
        let reader_batch_size = if stream_files_during_discovery {
            MANIFEST_STREAMED_INDEX_BATCH_SIZE
        } else {
            DISCOVERY_ENTRY_BATCH_SIZE
        };
        if tracing::enabled!(target: "lux::scan_performance", tracing::Level::DEBUG) {
            tracing::debug!(
                target: "lux::scan_performance",
                phase = "directory_read_budget",
                preparation_concurrency = preparation_concurrency as u64,
                directory_read_concurrency = 1_u64,
                directory_count = 1_u64,
                reader_batch_size = reader_batch_size as u64,
                "manifest directory reader concurrency budget"
            );
        }
        loop {
            let read_started = Instant::now();
            let reader_to_move = reader;
            let (next_reader, batch) =
                tokio::task::spawn_blocking(move || reader_to_move.next_batch(reader_batch_size))
                    .await
                    .map_err(|source| ScannerError::Io {
                        path: root_path.clone(),
                        source: std::io::Error::other(source.to_string()),
                    })??;
            reader = next_reader;
            let batch_file_count = batch
                .entries
                .iter()
                .filter(|entry| entry.entry_kind == "FILE")
                .count();
            record_manifest_scan_stage(
                "directory_batch_total",
                read_started,
                u64::try_from(batch.entries.len()).unwrap_or(u64::MAX),
                u64::try_from(batch_file_count).unwrap_or(u64::MAX),
                u64::try_from(batch.child_directories.len()).unwrap_or(u64::MAX),
            );
            record_manifest_scan_stage_duration(
                "directory_readdir",
                batch.readdir_duration,
                u64::try_from(batch.readdir_entry_count).unwrap_or(u64::MAX),
                0,
                0,
            );
            record_manifest_scan_stage_duration(
                "directory_stat",
                batch.stat_duration,
                u64::try_from(batch.stat_entry_count).unwrap_or(u64::MAX),
                u64::try_from(batch_file_count).unwrap_or(u64::MAX),
                u64::try_from(batch.child_directories.len()).unwrap_or(u64::MAX),
            );
            if cancellation.load(Ordering::Acquire) {
                return Ok(None);
            }
            let completed = batch.completed;
            let mut child_directories = batch.child_directories;
            let mut observations = batch.entries;
            result
                .child_directories
                .extend(child_directories.iter().cloned());
            if !completed && child_directories.is_empty() && observations.is_empty() {
                continue;
            }
            child_directories.sort_unstable();
            observations
                .sort_unstable_by(|left, right| left.relative_path.cmp(&right.relative_path));
            let file_paths = observations
                .iter()
                .filter(|entry| entry.entry_kind == "FILE")
                .map(|entry| entry.relative_path.clone())
                .collect::<Vec<_>>();
            let baseline_started = Instant::now();
            let baselines = if stream_files_during_discovery {
                self.database
                    .list_scan_manifest_filesystem_baselines(&root.id, &file_paths)
                    .await?
            } else {
                HashMap::new()
            };
            record_manifest_scan_stage(
                "baseline_query",
                baseline_started,
                u64::try_from(file_paths.len()).unwrap_or(u64::MAX),
                u64::try_from(file_paths.len()).unwrap_or(u64::MAX),
                0,
            );
            let (positive_indexes, unchanged_paths, seen_filesystem_entries) =
                if stream_files_during_discovery {
                    let Some((positive_indexes, unchanged_paths, seen_filesystem_entries)) = self
                        .prepare_manifest_discovery_positive_indexes(
                            ManifestPositiveIndexPreparationContext {
                                root,
                                relative_directory,
                                library_kind,
                                preparation_concurrency,
                                baselines: &baselines,
                                expected_root_identity,
                                expected_root_observation: &root_observation,
                                expected_directory_observation: &directory_observation,
                                entries: &observations,
                                cancellation,
                            },
                        )
                        .await?
                    else {
                        return Ok(None);
                    };
                    (positive_indexes, unchanged_paths, seen_filesystem_entries)
                } else {
                    (Vec::new(), Vec::new(), Vec::new())
                };
            let completed_directory = completed.then_some(relative_directory);
            let Some(commit_result) = self
                .commit_scan_manifest_discovery_chunk(
                    &NewScanManifestDiscoveryChunk {
                        manifest_id,
                        job_id,
                        library_root_id: &root.id,
                        child_directories: &child_directories,
                        entries: &observations,
                        positive_indexes: &positive_indexes,
                        unchanged_paths: &unchanged_paths,
                        seen_filesystem_entries: &seen_filesystem_entries,
                        completed_directory,
                    },
                    cancellation,
                    stream_files_during_discovery,
                    root_path.clone(),
                    root_observation.clone(),
                    directory_observation.clone(),
                )
                .await?
            else {
                return Ok(None);
            };
            result.observed_file_count = result.observed_file_count.saturating_add(
                usize::try_from(commit_result.observed_file_count).unwrap_or(usize::MAX),
            );
            result.created_items = result
                .created_items
                .saturating_add(commit_result.created_items);
            if completed {
                return Ok(Some(result));
            }
        }
    }

    async fn prepare_manifest_discovery_positive_indexes(
        &self,
        context: ManifestPositiveIndexPreparationContext<'_>,
    ) -> Result<
        Option<(
            Vec<NewScanManifestPositiveIndex>,
            Vec<String>,
            Vec<NewScanManifestSeenFilesystemEntry>,
        )>,
        ScannerError,
    > {
        let ManifestPositiveIndexPreparationContext {
            root,
            relative_directory,
            library_kind,
            preparation_concurrency,
            baselines,
            expected_root_identity,
            expected_root_observation,
            expected_directory_observation,
            entries,
            cancellation,
        } = context;
        let preparation_started = Instant::now();
        if !expected_root_observation.relative_path.is_empty()
            || expected_root_observation.entry_kind != "DIRECTORY"
            || expected_directory_observation.relative_path != relative_directory
            || expected_directory_observation.entry_kind != "DIRECTORY"
        {
            return Err(ScannerError::RootIdentityChanged(PathBuf::from(
                &root.canonical_path,
            )));
        }
        let (expected_root_device, expected_root_inode) = expected_root_identity
            .map(|(device, inode)| (Some(device), Some(inode)))
            .unwrap_or((
                expected_root_observation.device,
                expected_root_observation.inode,
            ));
        if !manifest_root_identity_matches(
            expected_root_device,
            expected_root_inode,
            expected_root_observation,
        ) {
            return Err(ScannerError::RootIdentityChanged(PathBuf::from(
                &root.canonical_path,
            )));
        }

        let root_path = PathBuf::from(&root.canonical_path);
        // The directory reader already captured and checked the root identity. Positive files
        // are re-statted through the secured directory handle below before they are committed,
        // so a second standalone stat(root) here only duplicates filesystem I/O.
        let mut classification_cache = MixedClassificationCache::default();
        let mut directory_folder_provider_ids: Option<Arc<OnceLock<BTreeMap<String, String>>>> =
            None;
        let mut unchanged_paths = Vec::new();
        let mut seen_filesystem_entries = Vec::new();
        let mut preparation_tasks = tokio::task::JoinSet::new();
        let preparation_concurrency = preparation_concurrency.max(1);
        let mut positive_indexes = Vec::new();
        let mut root_identity_lost = false;
        let mut classification_duration = Duration::ZERO;
        let mut file_preparation_duration = Duration::ZERO;
        let mut classification_count = 0_u64;
        let mut preparation_count = 0_u64;
        let mut active_preparation_tasks_peak = 0_usize;
        let measure_preparation =
            tracing::enabled!(target: "lux::scan_performance", tracing::Level::DEBUG);

        for observation in entries.iter().filter(|entry| entry.entry_kind == "FILE") {
            if cancellation.load(Ordering::Acquire) {
                return Ok(None);
            }
            let path = root_path.join(&observation.relative_path);
            let is_media = is_supported_movie_file(&path);
            let is_sidecar = is_supported_sidecar_file(&path);
            let baseline = baselines.get(&observation.relative_path);
            if let Some(baseline) = baseline {
                if baseline.entry_kind != "FILE" {
                    continue;
                }
                if !baseline.is_missing
                    && baseline.fingerprint.as_deref() == Some(observation.fingerprint.as_slice())
                {
                    let inferred_suffix = if is_media && library_kind == "MOVIE" {
                        infer_sibling_movie_variant_suffix(&path).await
                    } else {
                        None
                    };
                    let variant_identity_is_current = match inferred_suffix.as_deref() {
                        Some(suffix) => {
                            let base_item_id = self
                                .scanner
                                .find_inferred_movie_variant_base_item(
                                    &root.id, &root_path, &path, suffix,
                                )
                                .await?;
                            if let Some(base_item_id) = base_item_id {
                                self.database
                                    .scan_manifest_movie_variant_identity_is_current(
                                        &root.id,
                                        &observation.relative_path,
                                        &base_item_id,
                                        Some(suffix),
                                    )
                                    .await?
                            } else {
                                false
                            }
                        }
                        None => false,
                    };
                    if inferred_suffix.is_none() || variant_identity_is_current {
                        seen_filesystem_entries.push(NewScanManifestSeenFilesystemEntry {
                            filesystem_entry_id: baseline.id.clone(),
                            relative_path: observation.relative_path.clone(),
                            fingerprint: observation.fingerprint.clone(),
                        });
                        if is_media || is_sidecar {
                            unchanged_paths.push(observation.relative_path.clone());
                        }
                        continue;
                    }
                }
            }
            if !is_media && !is_sidecar {
                continue;
            }

            let delta_kind = match baseline {
                None => "ADD",
                Some(baseline) if baseline.is_missing => "REAPPEARED",
                Some(_) => "CHANGE",
            };
            let classification_started = measure_preparation.then(Instant::now);
            let filename_input = match library_kind {
                "MOVIE" => ManifestFilenameInput::MovieLibrary,
                "SERIES" => ManifestFilenameInput::SeriesLibrary,
                "HOMEVIDEOS" => ManifestFilenameInput::HomeVideos,
                _ if is_media => ManifestFilenameInput::PreparedMixed(
                    classify_mixed_manifest_file(&root_path, &path, &mut classification_cache)
                        .await,
                ),
                _ => ManifestFilenameInput::LegacyMixed(None),
            };
            if let Some(started) = classification_started {
                classification_duration = classification_duration.saturating_add(started.elapsed());
            }
            classification_count = classification_count.saturating_add(1);
            let movie_folder_provider_ids = if is_media && filename_input.is_movie() {
                Some(Arc::clone(
                    directory_folder_provider_ids.get_or_insert_with(|| Arc::new(OnceLock::new())),
                ))
            } else {
                None
            };
            let seed = ManifestPositiveIndexSeed {
                relative_path: observation.relative_path.clone(),
                delta_kind: delta_kind.to_owned(),
                base_filesystem_entry_id: baseline.map(|baseline| baseline.id.clone()),
                base_fingerprint: baseline.and_then(|baseline| baseline.fingerprint.clone()),
            };
            let scanner = self.scanner.clone();
            let root = root.clone();
            let task_root_path = root_path.clone();
            let observed = observation.clone();
            let is_add = delta_kind == "ADD";
            preparation_tasks.spawn(async move {
                let preparation_started = measure_preparation.then(Instant::now);
                let preparation = prepare_manifest_observation(
                    ManifestFilePreparationContext {
                        scanner,
                        root,
                        root_path: task_root_path,
                        expected_root_device,
                        expected_root_inode,
                        movie_folder_provider_ids_cache: movie_folder_provider_ids,
                        verify_path_after_preparation: false,
                    },
                    observed,
                    is_add,
                    None,
                    filename_input,
                )
                .await;
                let duration = preparation_started
                    .map(|started| started.elapsed())
                    .unwrap_or_default();
                (seed, preparation, duration)
            });
            preparation_count = preparation_count.saturating_add(1);
            if preparation_tasks.len() > active_preparation_tasks_peak {
                active_preparation_tasks_peak = preparation_tasks.len();
                record_manifest_scan_activity(active_preparation_tasks_peak, 0);
            }

            if preparation_tasks.len() >= preparation_concurrency {
                let prepared = preparation_tasks
                    .join_next()
                    .await
                    .ok_or_else(|| ScannerError::Io {
                        path: root_path.clone(),
                        source: std::io::Error::other("manifest preparation task set became empty"),
                    })?
                    .map_err(|error| ScannerError::Io {
                        path: root_path.clone(),
                        source: std::io::Error::other(error.to_string()),
                    })?;
                file_preparation_duration = file_preparation_duration.saturating_add(prepared.2);
                match manifest_discovery_index_from_preparation(prepared.0, prepared.1) {
                    ManifestDiscoveryIndexPreparation::Indexed(index) => {
                        positive_indexes.push(index)
                    }
                    ManifestDiscoveryIndexPreparation::Unstable => {}
                    ManifestDiscoveryIndexPreparation::RootIdentityChanged => {
                        root_identity_lost = true
                    }
                }
            }
        }

        while let Some(prepared) = preparation_tasks.join_next().await {
            if cancellation.load(Ordering::Acquire) {
                return Ok(None);
            }
            let prepared = prepared.map_err(|error| ScannerError::Io {
                path: root_path.clone(),
                source: std::io::Error::other(error.to_string()),
            })?;
            file_preparation_duration = file_preparation_duration.saturating_add(prepared.2);
            match manifest_discovery_index_from_preparation(prepared.0, prepared.1) {
                ManifestDiscoveryIndexPreparation::Indexed(index) => positive_indexes.push(index),
                ManifestDiscoveryIndexPreparation::Unstable => {}
                ManifestDiscoveryIndexPreparation::RootIdentityChanged => root_identity_lost = true,
            }
        }
        if root_identity_lost {
            return Err(ScannerError::RootIdentityChanged(root_path));
        }
        record_manifest_scan_stage_duration(
            "positive_classification",
            classification_duration,
            classification_count,
            classification_count,
            0,
        );
        record_manifest_scan_stage_duration(
            "positive_file_prepare",
            file_preparation_duration,
            preparation_count,
            preparation_count,
            0,
        );
        if !positive_indexes.is_empty() {
            let observations_by_path = entries
                .iter()
                .map(|observation| (observation.relative_path.as_str(), observation))
                .collect::<HashMap<_, _>>();
            let expected_files = positive_indexes
                .iter()
                .map(|positive| {
                    observations_by_path
                        .get(positive.relative_path.as_str())
                        .map(|observation| (**observation).clone())
                        .ok_or_else(|| {
                            ScannerError::InvalidRelativePath(positive.relative_path.clone())
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let current_files = stat_manifest_directory_file_batch(
                root_path.clone(),
                expected_root_observation.clone(),
                expected_directory_observation.clone(),
                expected_files.clone(),
            )
            .await?;
            positive_indexes = positive_indexes
                .into_iter()
                .zip(expected_files.iter())
                .zip(current_files)
                .filter_map(|((positive, expected), current)| {
                    current
                        .as_ref()
                        .is_some_and(|current| manifest_file_observation_matches(expected, current))
                        .then_some(positive)
                })
                .collect();
        }
        record_manifest_scan_stage(
            "positive_prepare_wall",
            preparation_started,
            preparation_count,
            preparation_count,
            0,
        );
        if tracing::enabled!(target: "lux::scan_performance", tracing::Level::DEBUG) {
            tracing::debug!(
                target: "lux::scan_performance",
                phase = "positive_prepare",
                application_ms = preparation_started.elapsed().as_millis() as u64,
                transaction_ms = 0_u64,
                positive_index_count = positive_indexes.len() as u64,
                unchanged_count = unchanged_paths.len() as u64,
                "manifest positive discovery preparation timing"
            );
        }
        Ok(Some((
            positive_indexes,
            unchanged_paths,
            seen_filesystem_entries,
        )))
    }

    async fn discover_scan_manifest_directory_group_batches(
        &self,
        context: ManifestRootDiscoveryContext<'_>,
        relative_directories: &[String],
    ) -> Result<Option<ManifestDiscoveryDirectoryResult>, ScannerError> {
        let root = context.root;
        let cancellation = context.cancellation;
        let stream_files_during_discovery = context.stream_files_during_discovery;
        let root_path = PathBuf::from(&root.canonical_path);
        let reader_batch_size = if stream_files_during_discovery {
            MANIFEST_STREAMED_INDEX_BATCH_SIZE
        } else {
            DISCOVERY_ENTRY_BATCH_SIZE
        };
        let pending_entry_limit = if stream_files_during_discovery {
            MANIFEST_STREAMED_ENTRY_BATCH_SIZE
        } else {
            DISCOVERY_ENTRY_BATCH_SIZE.saturating_add(1)
        };
        if tracing::enabled!(target: "lux::scan_performance", tracing::Level::DEBUG) {
            tracing::debug!(
                target: "lux::scan_performance",
                phase = "directory_read_budget",
                preparation_concurrency = context.preparation_concurrency as u64,
                directory_read_concurrency = 1_u64,
                directory_count = relative_directories.len() as u64,
                reader_batch_size = reader_batch_size as u64,
                "manifest directory reader concurrency budget"
            );
        }

        let mut result = ManifestDiscoveryDirectoryResult::default();
        let mut pending_chunks = Vec::new();
        let mut pending_entry_count = 0_usize;
        let mut pending_file_count = 0_usize;
        let mut pending_child_count = 0_usize;
        for relative_directory in relative_directories {
            if cancellation.load(Ordering::Acquire) {
                return Ok(None);
            }
            let relative = Path::new(relative_directory);
            if relative.is_absolute()
                || relative.components().any(|component| {
                    matches!(
                        component,
                        Component::CurDir
                            | Component::ParentDir
                            | Component::RootDir
                            | Component::Prefix(_)
                    )
                })
            {
                return Err(ScannerError::InvalidRelativePath(
                    relative_directory.clone(),
                ));
            }
            let open_root_path = root_path.clone();
            let open_relative_directory = relative_directory.clone();
            let open_started = Instant::now();
            let mut reader = tokio::task::spawn_blocking(move || {
                ManifestDirectoryReader::open(&open_root_path, &open_relative_directory)
            })
            .await
            .map_err(|source| ScannerError::Io {
                path: root_path.clone(),
                source: std::io::Error::other(source.to_string()),
            })??;
            record_manifest_scan_stage("directory_open", open_started, 1, 0, 1);
            record_manifest_scan_activity(0, 1);
            let root_observation = reader.root_observation.clone();
            let directory_observation = reader.directory_observation.clone();

            loop {
                if cancellation.load(Ordering::Acquire) {
                    return Ok(None);
                }
                let read_started = Instant::now();
                let (next_reader, batch) =
                    tokio::task::spawn_blocking(move || reader.next_batch(reader_batch_size))
                        .await
                        .map_err(|source| ScannerError::Io {
                            path: root_path.clone(),
                            source: std::io::Error::other(source.to_string()),
                        })??;
                reader = next_reader;
                let batch_file_count = batch
                    .entries
                    .iter()
                    .filter(|entry| entry.entry_kind == "FILE")
                    .count();
                record_manifest_scan_stage(
                    "directory_batch_total",
                    read_started,
                    u64::try_from(batch.entries.len()).unwrap_or(u64::MAX),
                    u64::try_from(batch_file_count).unwrap_or(u64::MAX),
                    u64::try_from(batch.child_directories.len()).unwrap_or(u64::MAX),
                );
                record_manifest_scan_stage_duration(
                    "directory_readdir",
                    batch.readdir_duration,
                    u64::try_from(batch.readdir_entry_count).unwrap_or(u64::MAX),
                    0,
                    0,
                );
                record_manifest_scan_stage_duration(
                    "directory_stat",
                    batch.stat_duration,
                    u64::try_from(batch.stat_entry_count).unwrap_or(u64::MAX),
                    u64::try_from(batch_file_count).unwrap_or(u64::MAX),
                    u64::try_from(batch.child_directories.len()).unwrap_or(u64::MAX),
                );
                if cancellation.load(Ordering::Acquire) {
                    return Ok(None);
                }
                if !batch.completed
                    && batch.child_directories.is_empty()
                    && batch.entries.is_empty()
                {
                    continue;
                }

                let mut child_directories = batch.child_directories;
                let mut entries = batch.entries;
                result
                    .child_directories
                    .extend(child_directories.iter().cloned());
                child_directories.sort_unstable();
                entries
                    .sort_unstable_by(|left, right| left.relative_path.cmp(&right.relative_path));
                let next_entry_count = pending_entry_count.saturating_add(entries.len());
                let next_file_count = pending_file_count.saturating_add(batch_file_count);
                let next_child_count = pending_child_count.saturating_add(child_directories.len());
                if !pending_chunks.is_empty()
                    && (next_entry_count > pending_entry_limit
                        || stream_files_during_discovery
                            && next_file_count > MANIFEST_STREAMED_INDEX_BATCH_SIZE
                        || next_child_count > DISCOVERY_CHILD_DIRECTORY_BATCH_SIZE)
                {
                    let Some(commit_result) = self
                        .commit_pending_manifest_directory_chunks(context, &pending_chunks)
                        .await?
                    else {
                        return Ok(None);
                    };
                    result.observed_file_count = result.observed_file_count.saturating_add(
                        usize::try_from(commit_result.observed_file_count).unwrap_or(usize::MAX),
                    );
                    result.created_items = result
                        .created_items
                        .saturating_add(commit_result.created_items);
                    pending_chunks.clear();
                    pending_entry_count = 0;
                    pending_file_count = 0;
                    pending_child_count = 0;
                }

                pending_entry_count = pending_entry_count.saturating_add(entries.len());
                pending_file_count = pending_file_count.saturating_add(batch_file_count);
                pending_child_count = pending_child_count.saturating_add(child_directories.len());
                pending_chunks.push(PendingManifestDirectoryChunk {
                    relative_directory: relative_directory.clone(),
                    root_observation: root_observation.clone(),
                    directory_observation: directory_observation.clone(),
                    child_directories,
                    entries,
                    completed_directory: batch.completed.then(|| relative_directory.clone()),
                });
                if pending_entry_count >= pending_entry_limit
                    || stream_files_during_discovery
                        && pending_file_count >= MANIFEST_STREAMED_INDEX_BATCH_SIZE
                    || pending_child_count >= DISCOVERY_CHILD_DIRECTORY_BATCH_SIZE
                {
                    let Some(commit_result) = self
                        .commit_pending_manifest_directory_chunks(context, &pending_chunks)
                        .await?
                    else {
                        return Ok(None);
                    };
                    result.observed_file_count = result.observed_file_count.saturating_add(
                        usize::try_from(commit_result.observed_file_count).unwrap_or(usize::MAX),
                    );
                    result.created_items = result
                        .created_items
                        .saturating_add(commit_result.created_items);
                    pending_chunks.clear();
                    pending_entry_count = 0;
                    pending_file_count = 0;
                    pending_child_count = 0;
                }
                if batch.completed {
                    break;
                }
            }
        }
        if !pending_chunks.is_empty() {
            let Some(commit_result) = self
                .commit_pending_manifest_directory_chunks(context, &pending_chunks)
                .await?
            else {
                return Ok(None);
            };
            result.observed_file_count = result.observed_file_count.saturating_add(
                usize::try_from(commit_result.observed_file_count).unwrap_or(usize::MAX),
            );
            result.created_items = result
                .created_items
                .saturating_add(commit_result.created_items);
        }
        Ok(Some(result))
    }

    async fn commit_pending_manifest_directory_chunks(
        &self,
        context: ManifestRootDiscoveryContext<'_>,
        chunks: &[PendingManifestDirectoryChunk],
    ) -> Result<Option<ManifestDiscoveryCommitResult>, ScannerError> {
        let ManifestRootDiscoveryContext {
            job_id,
            manifest_id,
            root,
            cancellation,
            stream_files_during_discovery,
            skip_baseline_queries,
            library_kind,
            preparation_concurrency,
            expected_root_identity,
        } = context;
        let file_paths = chunks
            .iter()
            .flat_map(|chunk| chunk.entries.iter())
            .filter(|entry| entry.entry_kind == "FILE")
            .map(|entry| entry.relative_path.clone())
            .collect::<Vec<_>>();
        let baseline_started = Instant::now();
        let baselines =
            if stream_files_during_discovery && !skip_baseline_queries && !file_paths.is_empty() {
                self.database
                    .list_scan_manifest_filesystem_baselines(&root.id, &file_paths)
                    .await?
            } else {
                HashMap::new()
            };
        record_manifest_scan_stage(
            "baseline_query",
            baseline_started,
            u64::try_from(file_paths.len()).unwrap_or(u64::MAX),
            u64::try_from(file_paths.len()).unwrap_or(u64::MAX),
            0,
        );
        let mut positive_indexes_by_chunk = Vec::with_capacity(chunks.len());
        let mut unchanged_paths_by_chunk = Vec::with_capacity(chunks.len());
        let mut seen_filesystem_entries_by_chunk = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            if cancellation.load(Ordering::Acquire) {
                return Ok(None);
            }
            if stream_files_during_discovery {
                let Some((positive_indexes, unchanged_paths, seen_filesystem_entries)) = self
                    .prepare_manifest_discovery_positive_indexes(
                        ManifestPositiveIndexPreparationContext {
                            root,
                            relative_directory: &chunk.relative_directory,
                            library_kind,
                            preparation_concurrency,
                            baselines: &baselines,
                            expected_root_identity,
                            expected_root_observation: &chunk.root_observation,
                            expected_directory_observation: &chunk.directory_observation,
                            entries: &chunk.entries,
                            cancellation,
                        },
                    )
                    .await?
                else {
                    return Ok(None);
                };
                positive_indexes_by_chunk.push(positive_indexes);
                unchanged_paths_by_chunk.push(unchanged_paths);
                seen_filesystem_entries_by_chunk.push(seen_filesystem_entries);
            } else {
                positive_indexes_by_chunk.push(Vec::new());
                unchanged_paths_by_chunk.push(Vec::new());
                seen_filesystem_entries_by_chunk.push(Vec::new());
            }
        }
        if stream_files_during_discovery {
            let observations = chunks
                .iter()
                .zip(&positive_indexes_by_chunk)
                .filter(|(_, positive_indexes)| positive_indexes.is_empty())
                .map(|(chunk, _)| {
                    (
                        chunk.root_observation.clone(),
                        chunk.directory_observation.clone(),
                    )
                })
                .collect::<Vec<_>>();
            if !observations.is_empty() {
                if cancellation.load(Ordering::Acquire) {
                    return Ok(None);
                }
                verify_manifest_directory_observations(
                    PathBuf::from(&root.canonical_path),
                    observations,
                )
                .await?;
            }
        }
        let stored_chunks = chunks
            .iter()
            .zip(&positive_indexes_by_chunk)
            .zip(&unchanged_paths_by_chunk)
            .zip(&seen_filesystem_entries_by_chunk)
            .map(
                |(((chunk, positive_indexes), unchanged_paths), seen_filesystem_entries)| {
                    NewScanManifestDiscoveryChunk {
                        manifest_id,
                        job_id,
                        library_root_id: &root.id,
                        child_directories: &chunk.child_directories,
                        entries: &chunk.entries,
                        positive_indexes,
                        unchanged_paths,
                        seen_filesystem_entries,
                        completed_directory: chunk.completed_directory.as_deref(),
                    }
                },
            )
            .collect::<Vec<_>>();
        if cancellation.load(Ordering::Acquire) {
            return Ok(None);
        }
        let has_positive_indexes = stored_chunks
            .iter()
            .any(|chunk| !chunk.positive_indexes.is_empty());
        let legacy_variant_paths = stored_chunks
            .iter()
            .flat_map(|chunk| chunk.positive_indexes.iter())
            .filter(|positive| positive.delta_kind != "ADD")
            .filter_map(|positive| match &positive.file {
                NewScanManifestIndexedFile::Movie(file) => Some(file.relative_path.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let transaction_started = Instant::now();
        let commit_result = self
            .database
            .commit_scan_manifest_discovery_chunks(&stored_chunks)
            .await;
        if has_positive_indexes
            && tracing::enabled!(target: "lux::scan_performance", tracing::Level::DEBUG)
        {
            tracing::debug!(
                target: "lux::scan_performance",
                phase = "positive_commit",
                application_ms = 0_u64,
                transaction_ms = transaction_started.elapsed().as_millis() as u64,
                "manifest positive discovery commit timing"
            );
        }
        match commit_result {
            Ok(result) => {
                let movie_identity_reassigned = self
                    .reconcile_legacy_movie_variant_paths(
                        job_id,
                        &root.id,
                        Path::new(&root.canonical_path),
                        &legacy_variant_paths,
                    )
                    .await?;
                self.publish_home_after_manifest_commit(&result, movie_identity_reassigned)
                    .await;
                if result.local_metadata_batches_changed {
                    self.notify_local_metadata_outbox_worker();
                }
                if result.metadata_targets_changed {
                    self.notify_local_metadata_worker(job_id);
                }
                Ok(Some(result))
            }
            Err(error) => {
                if cancellation.load(Ordering::Acquire)
                    || self.database.scan_job_cancel_requested(job_id).await?
                {
                    Ok(None)
                } else {
                    Err(error.into())
                }
            }
        }
    }

    async fn reconcile_legacy_movie_variant_paths(
        &self,
        job_id: &str,
        library_root_id: &str,
        root_path: &Path,
        relative_paths: &[String],
    ) -> Result<bool, ScannerError> {
        if relative_paths.is_empty() {
            return Ok(false);
        }
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Ok(false);
        };
        let Some(root) = self.database.find_library_root(library_root_id).await? else {
            return Ok(false);
        };
        let mut movie_identity_reassigned = false;
        for relative_path in relative_paths {
            let path = root_path.join(relative_path);
            let inferred_suffix = infer_sibling_movie_variant_suffix(&path).await;
            if inferred_suffix.is_none() {
                continue;
            }
            let report = self
                .scanner
                .scan_movie_file_with_inferred_suffix(
                    &job.library_id,
                    &root,
                    root_path,
                    &path,
                    &job.generation,
                    inferred_suffix.as_deref(),
                )
                .await?;
            movie_identity_reassigned |= report.changed_files > 0;
        }
        Ok(movie_identity_reassigned)
    }

    async fn commit_scan_manifest_discovery_chunk(
        &self,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
        cancellation: &AtomicBool,
        stream_files_during_discovery: bool,
        root_path: PathBuf,
        root_observation: NewScanManifestEntry,
        directory_observation: NewScanManifestEntry,
    ) -> Result<Option<ManifestDiscoveryCommitResult>, ScannerError> {
        if stream_files_during_discovery && chunk.positive_indexes.is_empty() {
            verify_manifest_directory_observation(
                root_path.clone(),
                root_observation,
                directory_observation,
            )
            .await?;
        }
        if cancellation.load(Ordering::Acquire) {
            return Ok(None);
        }
        let has_positive_indexes = !chunk.positive_indexes.is_empty();
        let transaction_started = Instant::now();
        let commit_result = self
            .database
            .commit_scan_manifest_discovery_chunks(std::slice::from_ref(chunk))
            .await;
        if has_positive_indexes
            && tracing::enabled!(target: "lux::scan_performance", tracing::Level::DEBUG)
        {
            tracing::debug!(
                target: "lux::scan_performance",
                phase = "positive_commit",
                application_ms = 0_u64,
                transaction_ms = transaction_started.elapsed().as_millis() as u64,
                "manifest positive discovery commit timing"
            );
        }
        match commit_result {
            Ok(result) => {
                let legacy_variant_paths = chunk
                    .positive_indexes
                    .iter()
                    .filter(|positive| positive.delta_kind != "ADD")
                    .filter_map(|positive| match &positive.file {
                        NewScanManifestIndexedFile::Movie(file) => Some(file.relative_path.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let movie_identity_reassigned = self
                    .reconcile_legacy_movie_variant_paths(
                        chunk.job_id,
                        chunk.library_root_id,
                        &root_path,
                        &legacy_variant_paths,
                    )
                    .await?;
                self.publish_home_after_manifest_commit(&result, movie_identity_reassigned)
                    .await;
                if result.local_metadata_batches_changed {
                    self.notify_local_metadata_outbox_worker();
                }
                if result.metadata_targets_changed {
                    self.notify_local_metadata_worker(chunk.job_id);
                }
                Ok(Some(result))
            }
            Err(error) => {
                if cancellation.load(Ordering::Acquire)
                    || self
                        .database
                        .scan_job_cancel_requested(chunk.job_id)
                        .await?
                {
                    Ok(None)
                } else {
                    Err(error.into())
                }
            }
        }
    }

    async fn publish_home_after_manifest_commit(
        &self,
        result: &ManifestDiscoveryCommitResult,
        additional_content_changed: bool,
    ) {
        if result.created_items == 0
            && !result.local_metadata_batches_changed
            && !result.metadata_targets_changed
            && !additional_content_changed
        {
            return;
        }
        if let Some(home) = &self.home {
            home.invalidate();
        }
        self.user_events.publish_home_coalesced().await;
    }

    async fn run_lite_scan_manifest_discovery_batch(
        &self,
        job: &StoredScanJob,
        manifest_id: &str,
        batch_size: usize,
        cancellation: &AtomicBool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        self.ensure_lite_manifest_discovery_session(manifest_id)
            .await?;
        let directories = self.pop_lite_manifest_directories(
            manifest_id,
            batch_size.min(MANIFEST_DISCOVERY_BATCH_SIZE),
        );
        let library = self
            .database
            .find_library(&job.library_id)
            .await?
            .ok_or(ScanJobError::LibraryNotFound)?;
        let preparation_concurrency = self
            .effective_scan_concurrency(configured_scan_concurrency(
                self.scan_concurrency_override,
                Some(library.scan_concurrency),
                self.default_scan_concurrency,
            ))
            .await
            .max(1);
        let mut directories_by_root = BTreeMap::<String, (bool, Vec<String>)>::new();
        for (root_id, relative_directory, has_filesystem_entries) in directories {
            directories_by_root
                .entry(root_id)
                .or_insert_with(|| (has_filesystem_entries, Vec::new()))
                .1
                .push(relative_directory);
        }
        let roots_by_id = self
            .database
            .list_library_roots_by_ids(&directories_by_root.keys().cloned().collect::<Vec<_>>())
            .await?;
        let mut discovered_count = job.total_count;
        let mut created_items = 0_usize;

        for (root_id, (has_filesystem_entries, relative_directories)) in directories_by_root {
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(&job.id).await;
            }
            let Some(root) = roots_by_id.get(&root_id).cloned() else {
                self.database
                    .mark_scan_manifest_root_unavailable(manifest_id, &root_id)
                    .await?;
                self.remove_lite_manifest_root(manifest_id, &root_id);
                continue;
            };
            let expected_root_identity = self
                .database
                .get_scan_manifest_root_identity(manifest_id, &root.id)
                .await?
                .and_then(|(_, device, inode)| device.zip(inode));
            let context = ManifestRootDiscoveryContext {
                job_id: &job.id,
                manifest_id,
                root: &root,
                cancellation,
                stream_files_during_discovery: true,
                skip_baseline_queries: !has_filesystem_entries,
                library_kind: &library.kind,
                preparation_concurrency,
                expected_root_identity,
            };
            let discovery = self
                .discover_scan_manifest_directory_group_batches(context, &relative_directories)
                .await;
            match discovery {
                Ok(Some(discovered)) => {
                    discovered_count = discovered_count.saturating_add(
                        i64::try_from(discovered.observed_file_count).unwrap_or(i64::MAX),
                    );
                    created_items = created_items.saturating_add(discovered.created_items);
                    self.push_lite_manifest_directories(
                        manifest_id,
                        &root_id,
                        has_filesystem_entries,
                        discovered.child_directories,
                    );
                    if !root.is_available {
                        self.database
                            .update_library_root_availability(&root.id, true)
                            .await?;
                    }
                }
                Ok(None) => return self.cancel_running_job(&job.id).await,
                Err(_error)
                    if self
                        .cancellation_requested(&job.id, false, cancellation)
                        .await? =>
                {
                    return self.cancel_running_job(&job.id).await;
                }
                Err(ScannerError::Io { .. } | ScannerError::RootIdentityChanged(_)) => {
                    self.database
                        .update_library_root_availability(&root.id, false)
                        .await?;
                    self.database
                        .mark_scan_manifest_root_unavailable(manifest_id, &root.id)
                        .await?;
                    self.remove_lite_manifest_root(manifest_id, &root.id);
                    self.record_event(
                        &job.id,
                        "WARN",
                        "ROOT_UNAVAILABLE",
                        "媒体库根路径不可用，已跳过本轮缺失判定",
                        "{}",
                    )
                    .await;
                }
                Err(error) => {
                    return self
                        .fail_reconciliation_job(job, error, &[], job.processed_count)
                        .await;
                }
            }
        }

        if self
            .cancellation_requested(&job.id, false, cancellation)
            .await?
        {
            return self.cancel_running_job(&job.id).await;
        }
        if !self.lite_manifest_has_pending_directories(manifest_id) {
            self.database
                .finish_lite_scan_manifest_roots(manifest_id)
                .await?;
            let index_completion_started = Instant::now();
            let total = self
                .database
                .finish_scan_manifest_discovery(manifest_id, &job.id)
                .await?;
            record_manifest_scan_stage(
                "index_completion",
                index_completion_started,
                u64::try_from(total).unwrap_or(u64::MAX),
                u64::try_from(total).unwrap_or(u64::MAX),
                0,
            );
            self.clear_lite_manifest_discovery_session(manifest_id);
            self.record_event(
                &job.id,
                "INFO",
                "DISCOVERY_COMPLETED",
                "媒体库目录发现完成",
                &format!(r#"{{"discovered":{total},"discoveryCompleted":true}}"#),
            )
            .await;
        } else {
            self.record_event(
                &job.id,
                "INFO",
                "DISCOVERY_PROGRESS",
                "媒体库目录发现进行中",
                &format!(r#"{{"discovered":{discovered_count},"discoveryCompleted":false}}"#),
            )
            .await;
        }
        Ok(ScanBatchReport {
            status: "RUNNING".to_owned(),
            processed: usize::try_from(discovered_count.saturating_sub(job.total_count))
                .unwrap_or(usize::MAX),
            created_items,
            completed: false,
        })
    }

    async fn run_scan_manifest_discovery_batch(
        &self,
        job: &StoredScanJob,
        manifest_id: &str,
        batch_size: usize,
        cancellation: &AtomicBool,
        stream_files_during_discovery: bool,
        lite_mode: bool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        if lite_mode {
            return self
                .run_lite_scan_manifest_discovery_batch(job, manifest_id, batch_size, cancellation)
                .await;
        }
        let limit =
            i64::try_from(batch_size.min(MANIFEST_DISCOVERY_BATCH_SIZE)).unwrap_or(i64::MAX);
        let directories = self
            .database
            .list_scan_manifest_directories(manifest_id, limit)
            .await?;
        let library = self
            .database
            .find_library(&job.library_id)
            .await?
            .ok_or(ScanJobError::LibraryNotFound)?;
        let library_kind = library.kind;
        let preparation_concurrency = self
            .effective_scan_concurrency(configured_scan_concurrency(
                self.scan_concurrency_override,
                Some(library.scan_concurrency),
                self.default_scan_concurrency,
            ))
            .await
            .max(1);
        self.update_activity(
            &job.id,
            directories
                .last()
                .map(|directory| directory.relative_path.as_str()),
            "DISCOVERY",
        )
        .await?;
        let mut directories_by_root = BTreeMap::<String, Vec<String>>::new();
        for directory in directories {
            directories_by_root
                .entry(directory.library_root_id)
                .or_default()
                .push(directory.relative_path);
        }
        let root_ids = directories_by_root.keys().cloned().collect::<Vec<_>>();
        let roots_by_id = self.database.list_library_roots_by_ids(&root_ids).await?;
        let mut discovered_count = job.total_count;
        let mut created_items = 0_usize;
        for (root_id, relative_directories) in directories_by_root {
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(&job.id).await;
            }
            let Some(root) = roots_by_id.get(&root_id).cloned() else {
                self.database
                    .mark_scan_manifest_root_unavailable(manifest_id, &root_id)
                    .await?;
                self.database
                    .discard_reconciliation_root_entries(&job.id, &root_id)
                    .await?;
                continue;
            };
            let expected_root_identity = self
                .database
                .get_scan_manifest_root_identity(manifest_id, &root.id)
                .await?
                .and_then(|(_, device, inode)| device.zip(inode));
            let discovery_context = ManifestRootDiscoveryContext {
                job_id: &job.id,
                manifest_id,
                root: &root,
                cancellation,
                stream_files_during_discovery,
                skip_baseline_queries: false,
                library_kind: &library_kind,
                preparation_concurrency,
                expected_root_identity,
            };
            let discovered = if relative_directories.len() == 1 {
                self.discover_scan_manifest_directory_batches(
                    discovery_context,
                    &relative_directories[0],
                )
                .await
            } else {
                self.discover_scan_manifest_directory_group_batches(
                    discovery_context,
                    &relative_directories,
                )
                .await
            };
            match discovered {
                Ok(Some(discovered)) => {
                    discovered_count = discovered_count.saturating_add(
                        i64::try_from(discovered.observed_file_count).unwrap_or(i64::MAX),
                    );
                    created_items = created_items.saturating_add(discovered.created_items);
                    if !root.is_available {
                        self.database
                            .update_library_root_availability(&root.id, true)
                            .await?;
                    }
                }
                Ok(None) => return self.cancel_running_job(&job.id).await,
                Err(_error)
                    if self
                        .cancellation_requested(&job.id, false, cancellation)
                        .await? =>
                {
                    return self.cancel_running_job(&job.id).await;
                }
                Err(ScannerError::Io { .. } | ScannerError::RootIdentityChanged(_)) => {
                    self.database
                        .update_library_root_availability(&root.id, false)
                        .await?;
                    self.database
                        .mark_scan_manifest_root_unavailable(manifest_id, &root.id)
                        .await?;
                    self.database
                        .discard_reconciliation_root_entries(&job.id, &root.id)
                        .await?;
                    self.record_event(
                        &job.id,
                        "WARN",
                        "ROOT_UNAVAILABLE",
                        "媒体库根路径不可用，已跳过本轮缺失判定",
                        "{}",
                    )
                    .await;
                }
                Err(error) => {
                    return self
                        .fail_reconciliation_job(job, error, &[], job.processed_count)
                        .await;
                }
            }
        }

        if self
            .cancellation_requested(&job.id, false, cancellation)
            .await?
        {
            return self.cancel_running_job(&job.id).await;
        }
        let remaining = self
            .database
            .list_scan_manifest_directories(manifest_id, 1)
            .await?;
        if remaining.is_empty() {
            let index_completion_started = Instant::now();
            let total = match self
                .database
                .finish_scan_manifest_discovery(manifest_id, &job.id)
                .await
            {
                Ok(total) => {
                    record_manifest_scan_stage(
                        "index_completion",
                        index_completion_started,
                        u64::try_from(total).unwrap_or(u64::MAX),
                        u64::try_from(total).unwrap_or(u64::MAX),
                        0,
                    );
                    total
                }
                Err(_error)
                    if self
                        .cancellation_requested(&job.id, false, cancellation)
                        .await? =>
                {
                    return self.cancel_running_job(&job.id).await;
                }
                Err(error) => return Err(error.into()),
            };
            let details = format!(r#"{{"discovered":{total},"discoveryCompleted":true}}"#);
            self.record_event(
                &job.id,
                "INFO",
                "DISCOVERY_COMPLETED",
                "媒体库目录发现完成",
                &details,
            )
            .await;
        } else {
            let details =
                format!(r#"{{"discovered":{discovered_count},"discoveryCompleted":false}}"#);
            self.record_event(
                &job.id,
                "INFO",
                "DISCOVERY_PROGRESS",
                "媒体库目录发现进行中",
                &details,
            )
            .await;
        }
        if stream_files_during_discovery {
            return Ok(ScanBatchReport {
                status: "RUNNING".to_owned(),
                processed: usize::try_from(discovered_count.saturating_sub(job.total_count))
                    .unwrap_or(usize::MAX),
                created_items,
                completed: false,
            });
        }
        Ok(ScanBatchReport {
            status: "RUNNING".to_owned(),
            processed: 0,
            created_items: 0,
            completed: false,
        })
    }

    async fn run_scan_manifest_diff_batch(
        &self,
        job: &StoredScanJob,
        manifest_id: &str,
        cancellation: &AtomicBool,
        positives_already_indexed: bool,
        discovery_format_version: i64,
        lite_mode: bool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        for root in self.database.list_library_roots(&job.library_id).await? {
            if self
                .cancellation_requested(&job.id, false, cancellation)
                .await?
            {
                return self.cancel_running_job(&job.id).await;
            }
            let Some((state, expected_device, expected_inode)) = self
                .database
                .get_scan_manifest_root_identity(manifest_id, &root.id)
                .await?
            else {
                continue;
            };
            if state != "COMPLETE" {
                continue;
            }
            let root_path = PathBuf::from(&root.canonical_path);
            let root_matches_snapshot = match stat_manifest_root(root_path).await {
                Ok(observed) => {
                    manifest_root_identity_matches(expected_device, expected_inode, &observed)
                }
                Err(_) => false,
            };
            if !root_matches_snapshot {
                self.database
                    .mark_scan_manifest_root_unavailable(manifest_id, &root.id)
                    .await?;
                self.record_event(
                    &job.id,
                    "WARN",
                    "ROOT_UNAVAILABLE",
                    "媒体库根目录在发现完成后发生变化，已跳过该根路径的差异应用",
                    "{}",
                )
                .await;
            }
        }
        self.update_activity(&job.id, Some("文件差异计算"), "FINALIZING")
            .await?;
        let page_size = i64::try_from(MANIFEST_DIFF_BATCH_SIZE).unwrap_or(i64::MAX);
        let mut after_library_root_id: Option<String> = None;
        let mut after_relative_path: Option<String> = None;
        if !positives_already_indexed {
            loop {
                if self
                    .cancellation_requested(&job.id, false, cancellation)
                    .await?
                {
                    return self.cancel_running_job(&job.id).await;
                }
                let candidates = self
                    .database
                    .list_scan_manifest_diff_candidates(
                        manifest_id,
                        after_library_root_id.as_deref(),
                        after_relative_path.as_deref(),
                        page_size,
                    )
                    .await?;
                let Some(last_candidate) = candidates.last() else {
                    break;
                };
                let delta_ids = candidates
                    .iter()
                    .map(|_| Uuid::now_v7().to_string())
                    .collect::<Vec<_>>();
                let mut deltas = Vec::with_capacity(candidates.len());
                for (candidate, id) in candidates.iter().zip(&delta_ids) {
                    if candidate.delta_kind != "ADD"
                        && (candidate.base_filesystem_entry_id.is_none()
                            || candidate.base_entry_kind.as_deref() != Some("FILE"))
                    {
                        return Err(StorageError::Conflict(
                            "manifest difference has an invalid filesystem baseline".to_owned(),
                        )
                        .into());
                    }
                    deltas.push(NewScanManifestDelta {
                        id,
                        library_root_id: &candidate.library_root_id,
                        relative_path: &candidate.relative_path,
                        observation_sequence: Some(candidate.observation_sequence),
                        delta_kind: &candidate.delta_kind,
                        base_filesystem_entry_id: candidate.base_filesystem_entry_id.as_deref(),
                        base_fingerprint: candidate.base_fingerprint.as_deref(),
                    });
                }
                self.database
                    .insert_scan_manifest_deltas(manifest_id, &deltas)
                    .await?;
                after_library_root_id = Some(last_candidate.library_root_id.clone());
                after_relative_path = Some(last_candidate.relative_path.clone());
            }
        }

        if discovery_format_version == 3 {
            let mut removal_after_library_root_id: Option<String> = None;
            let mut removal_after_relative_path: Option<String> = None;
            if lite_mode {
                loop {
                    if self
                        .cancellation_requested(&job.id, false, cancellation)
                        .await?
                    {
                        return self.cancel_running_job(&job.id).await;
                    }
                    let candidates = self
                        .database
                        .list_scan_manifest_removal_candidates(
                            manifest_id,
                            discovery_format_version,
                            false,
                            removal_after_library_root_id.as_deref(),
                            removal_after_relative_path.as_deref(),
                            page_size,
                        )
                        .await?;
                    let Some(last_candidate) = candidates.last() else {
                        break;
                    };
                    let delta_ids = candidates
                        .iter()
                        .map(|_| Uuid::now_v7().to_string())
                        .collect::<Vec<_>>();
                    let deltas = candidates
                        .iter()
                        .zip(&delta_ids)
                        .map(|(candidate, id)| NewScanManifestDelta {
                            id,
                            library_root_id: &candidate.library_root_id,
                            relative_path: &candidate.relative_path,
                            observation_sequence: None,
                            delta_kind: "REMOVE",
                            base_filesystem_entry_id: Some(&candidate.base_filesystem_entry_id),
                            base_fingerprint: candidate.base_fingerprint.as_deref(),
                        })
                        .collect::<Vec<_>>();
                    self.database
                        .insert_scan_manifest_deltas(manifest_id, &deltas)
                        .await?;
                    removal_after_library_root_id = Some(last_candidate.library_root_id.clone());
                    removal_after_relative_path = Some(last_candidate.relative_path.clone());
                }
            } else {
                let mut directory_after_library_root_id: Option<String> = None;
                let mut directory_after_relative_path: Option<String> = None;
                loop {
                    if self
                        .cancellation_requested(&job.id, false, cancellation)
                        .await?
                    {
                        return self.cancel_running_job(&job.id).await;
                    }
                    let directories = self
                        .database
                        .list_scan_manifest_complete_directories(
                            manifest_id,
                            directory_after_library_root_id.as_deref(),
                            directory_after_relative_path.as_deref(),
                            MANIFEST_REMOVAL_DIRECTORY_BATCH_SIZE,
                        )
                        .await?;
                    let Some(last_directory) = directories.last() else {
                        break;
                    };
                    loop {
                        if self
                            .cancellation_requested(&job.id, false, cancellation)
                            .await?
                        {
                            return self.cancel_running_job(&job.id).await;
                        }
                        let candidates = self
                            .database
                            .list_scan_manifest_removal_candidates_for_directories(
                                manifest_id,
                                &directories,
                                page_size,
                            )
                            .await?;
                        if candidates.is_empty() {
                            break;
                        }
                        let delta_ids = candidates
                            .iter()
                            .map(|_| Uuid::now_v7().to_string())
                            .collect::<Vec<_>>();
                        let deltas = candidates
                            .iter()
                            .zip(&delta_ids)
                            .map(|(candidate, id)| NewScanManifestDelta {
                                id,
                                library_root_id: &candidate.library_root_id,
                                relative_path: &candidate.relative_path,
                                observation_sequence: None,
                                delta_kind: "REMOVE",
                                base_filesystem_entry_id: Some(&candidate.base_filesystem_entry_id),
                                base_fingerprint: candidate.base_fingerprint.as_deref(),
                            })
                            .collect::<Vec<_>>();
                        self.database
                            .insert_scan_manifest_deltas(manifest_id, &deltas)
                            .await?;
                    }
                    directory_after_library_root_id = Some(last_directory.library_root_id.clone());
                    directory_after_relative_path = Some(last_directory.relative_path.clone());
                }
                if self
                    .database
                    .scan_manifest_has_uncovered_files(manifest_id)
                    .await?
                {
                    let mut uncovered_after_library_root_id: Option<String> = None;
                    let mut uncovered_after_relative_path: Option<String> = None;
                    loop {
                        if self
                            .cancellation_requested(&job.id, false, cancellation)
                            .await?
                        {
                            return self.cancel_running_job(&job.id).await;
                        }
                        let candidates = self
                            .database
                            .list_scan_manifest_removal_candidates(
                                manifest_id,
                                discovery_format_version,
                                true,
                                uncovered_after_library_root_id.as_deref(),
                                uncovered_after_relative_path.as_deref(),
                                page_size,
                            )
                            .await?;
                        let Some(last_candidate) = candidates.last() else {
                            break;
                        };
                        let delta_ids = candidates
                            .iter()
                            .map(|_| Uuid::now_v7().to_string())
                            .collect::<Vec<_>>();
                        let deltas = candidates
                            .iter()
                            .zip(&delta_ids)
                            .map(|(candidate, id)| NewScanManifestDelta {
                                id,
                                library_root_id: &candidate.library_root_id,
                                relative_path: &candidate.relative_path,
                                observation_sequence: None,
                                delta_kind: "REMOVE",
                                base_filesystem_entry_id: Some(&candidate.base_filesystem_entry_id),
                                base_fingerprint: candidate.base_fingerprint.as_deref(),
                            })
                            .collect::<Vec<_>>();
                        self.database
                            .insert_scan_manifest_deltas(manifest_id, &deltas)
                            .await?;
                        uncovered_after_library_root_id =
                            Some(last_candidate.library_root_id.clone());
                        uncovered_after_relative_path = Some(last_candidate.relative_path.clone());
                    }
                }
            }
        } else {
            after_library_root_id = None;
            after_relative_path = None;
            loop {
                if self
                    .cancellation_requested(&job.id, false, cancellation)
                    .await?
                {
                    return self.cancel_running_job(&job.id).await;
                }
                let candidates = self
                    .database
                    .list_scan_manifest_removal_candidates(
                        manifest_id,
                        discovery_format_version,
                        false,
                        after_library_root_id.as_deref(),
                        after_relative_path.as_deref(),
                        page_size,
                    )
                    .await?;
                let Some(last_candidate) = candidates.last() else {
                    break;
                };
                let delta_ids = candidates
                    .iter()
                    .map(|_| Uuid::now_v7().to_string())
                    .collect::<Vec<_>>();
                let deltas = candidates
                    .iter()
                    .zip(&delta_ids)
                    .map(|(candidate, id)| NewScanManifestDelta {
                        id,
                        library_root_id: &candidate.library_root_id,
                        relative_path: &candidate.relative_path,
                        observation_sequence: None,
                        delta_kind: "REMOVE",
                        base_filesystem_entry_id: Some(&candidate.base_filesystem_entry_id),
                        base_fingerprint: candidate.base_fingerprint.as_deref(),
                    })
                    .collect::<Vec<_>>();
                self.database
                    .insert_scan_manifest_deltas(manifest_id, &deltas)
                    .await?;
                after_library_root_id = Some(last_candidate.library_root_id.clone());
                after_relative_path = Some(last_candidate.relative_path.clone());
            }
        }

        if !self
            .database
            .finish_scan_manifest_diff(manifest_id, &job.id)
            .await?
        {
            if self
                .cancellation_requested(&job.id, false, cancellation)
                .await?
            {
                return self.cancel_running_job(&job.id).await;
            }
            self.record_event(
                &job.id,
                "WARN",
                "MANIFEST_DIFF_RETRY",
                "扫描期间文件索引发生变化，将重新计算差异",
                "{}",
            )
            .await;
            return Ok(ScanBatchReport {
                status: "RUNNING".to_owned(),
                processed: 0,
                created_items: 0,
                completed: false,
            });
        }
        let manifest = self
            .database
            .get_scan_manifest(manifest_id)
            .await?
            .ok_or_else(|| StorageError::Conflict("scan manifest disappeared".to_owned()))?;
        self.record_event(
            &job.id,
            "INFO",
            "MANIFEST_DIFF_COMPLETED",
            "文件差异计算完成",
            &format!(
                r#"{{"add":{},"change":{},"remove":{},"reappeared":{},"unchanged":{}}}"#,
                manifest.add_count,
                manifest.change_count,
                manifest.remove_count,
                manifest.reappeared_count,
                manifest.unchanged_count
            ),
        )
        .await;
        Ok(ScanBatchReport {
            status: "RUNNING".to_owned(),
            processed: 0,
            created_items: 0,
            completed: false,
        })
    }

    async fn run_scan_manifest_apply_batch(
        &self,
        job: &StoredScanJob,
        manifest_id: &str,
        batch_size: usize,
        cancellation: &AtomicBool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let apply_started = Instant::now();
        let mut transaction_elapsed = Duration::ZERO;
        let deltas = self
            .database
            .list_pending_scan_manifest_deltas(
                manifest_id,
                i64::try_from(batch_size).unwrap_or(i64::MAX),
            )
            .await?;
        if deltas.is_empty() {
            return self.finish_scan_manifest_indexing(job, manifest_id).await;
        }
        self.update_activity(
            &job.id,
            deltas.last().map(|delta| delta.relative_path.as_str()),
            "INDEXING",
        )
        .await?;
        let library = self
            .database
            .find_library(&job.library_id)
            .await?
            .ok_or(ScanJobError::LibraryNotFound)?;
        let root_ids = deltas
            .iter()
            .map(|delta| delta.library_root_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let roots = self.database.list_library_roots_by_ids(&root_ids).await?;
        let mut processed = 0_usize;
        let mut created_items = 0_usize;
        for root_id in root_ids {
            if self
                .cancellation_requested(&job.id, false, cancellation)
                .await?
            {
                return self.cancel_running_job(&job.id).await;
            }
            let root_deltas = deltas
                .iter()
                .filter(|delta| delta.library_root_id == root_id)
                .cloned()
                .collect::<Vec<_>>();
            let Some(root) = roots.get(&root_id) else {
                let unstable_delta_ids = root_deltas
                    .iter()
                    .map(|delta| delta.id.clone())
                    .collect::<Vec<_>>();
                let transaction_started = Instant::now();
                let result = self
                    .database
                    .commit_scan_manifest_delta_batch(&ManifestDeltaBatchCommit {
                        job_id: &job.id,
                        manifest_id,
                        library_id: &job.library_id,
                        library_root_id: &root_id,
                        generation: &job.generation,
                        deltas: &root_deltas,
                        unstable_delta_ids: &unstable_delta_ids,
                        movie_files: &[],
                        episode_files: &[],
                        unresolved_files: &[],
                        sidecar_entries: &[],
                        removed_media_paths: &[],
                        removed_sidecar_paths: &[],
                    })
                    .await?;
                transaction_elapsed += transaction_started.elapsed();
                processed = processed.saturating_add(
                    result
                        .applied_count
                        .saturating_add(result.conflict_count)
                        .saturating_add(result.unstable_count),
                );
                continue;
            };
            let root_path = PathBuf::from(&root.canonical_path);
            let root_identity = self
                .database
                .get_scan_manifest_root_identity(manifest_id, &root_id)
                .await?;
            let root_is_same_object = match root_identity.as_ref() {
                Some((state, expected_device, expected_inode)) if state == "COMPLETE" => {
                    match stat_manifest_root(root_path.clone()).await {
                        Ok(observed) => manifest_root_identity_matches(
                            *expected_device,
                            *expected_inode,
                            &observed,
                        ),
                        Err(_) => false,
                    }
                }
                _ => false,
            };
            if !root_is_same_object {
                self.database
                    .mark_scan_manifest_root_unavailable(manifest_id, &root_id)
                    .await?;
                let unstable_delta_ids = root_deltas
                    .iter()
                    .map(|delta| delta.id.clone())
                    .collect::<Vec<_>>();
                let transaction_started = Instant::now();
                let result = self
                    .database
                    .commit_scan_manifest_delta_batch(&ManifestDeltaBatchCommit {
                        job_id: &job.id,
                        manifest_id,
                        library_id: &job.library_id,
                        library_root_id: &root_id,
                        generation: &job.generation,
                        deltas: &root_deltas,
                        unstable_delta_ids: &unstable_delta_ids,
                        movie_files: &[],
                        episode_files: &[],
                        unresolved_files: &[],
                        sidecar_entries: &[],
                        removed_media_paths: &[],
                        removed_sidecar_paths: &[],
                    })
                    .await?;
                transaction_elapsed += transaction_started.elapsed();
                processed = processed.saturating_add(
                    result
                        .applied_count
                        .saturating_add(result.conflict_count)
                        .saturating_add(result.unstable_count),
                );
                continue;
            }
            let (expected_root_device, expected_root_inode) = root_identity
                .as_ref()
                .map(|(_, device, inode)| (*device, *inode))
                .unwrap_or((None, None));
            let mut prepared_files = ManifestApplyPreparedFiles::default();
            let mut removed_media_paths = Vec::new();
            let mut removed_sidecar_paths = Vec::new();
            let mut removal_candidates = Vec::new();
            let mut mixed_cache = MixedClassificationCache::default();
            let mut root_identity_lost;

            let preparation_concurrency = self.scanner.scan_concurrency.max(1);
            let mut preparation_tasks: JoinSet<ManifestDeltaPreparation> = JoinSet::new();
            for delta in &root_deltas {
                if cancellation.load(Ordering::Acquire) {
                    return self.cancel_running_job(&job.id).await;
                }
                if delta.delta_kind == "REMOVE" {
                    removal_candidates.push(delta.clone());
                    continue;
                }
                if delta.fingerprint.is_none() {
                    prepared_files.unstable_delta_ids.push(delta.id.clone());
                    continue;
                }
                let path = root_path.join(&delta.relative_path);
                let classification = if is_supported_movie_file(Path::new(&delta.relative_path)) {
                    Some(match library.kind.as_str() {
                        "MOVIE" => {
                            if parse_movie_filename(
                                path.file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or_default(),
                            )
                            .is_some()
                            {
                                MixedClassification::Movie
                            } else {
                                MixedClassification::Unresolved
                            }
                        }
                        "SERIES" => {
                            if parse_episode_filename(
                                path.file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or_default(),
                            )
                            .is_some()
                            {
                                MixedClassification::Episode
                            } else {
                                MixedClassification::Unresolved
                            }
                        }
                        "HOMEVIDEOS" => MixedClassification::Unresolved,
                        _ => classify_mixed_file(&root_path, &path, &mut mixed_cache).await,
                    })
                } else {
                    None
                };
                if preparation_tasks.len() >= preparation_concurrency {
                    let preparation = preparation_tasks
                        .join_next()
                        .await
                        .ok_or_else(|| {
                            StorageError::Conflict(
                                "manifest preparation task set became empty".to_owned(),
                            )
                        })?
                        .map_err(|error| ScannerError::Io {
                            path: root_path.clone(),
                            source: std::io::Error::other(error.to_string()),
                        })?;
                    prepared_files.record(preparation);
                }
                let scanner = self.scanner.clone();
                let root = root.clone();
                let root_path = root_path.clone();
                let delta = delta.clone();
                let library_kind = library.kind.clone();
                preparation_tasks.spawn(async move {
                    prepare_manifest_delta(
                        ManifestFilePreparationContext {
                            scanner,
                            root,
                            root_path,
                            expected_root_device,
                            expected_root_inode,
                            movie_folder_provider_ids_cache: None,
                            verify_path_after_preparation: true,
                        },
                        delta,
                        classification,
                        &library_kind,
                    )
                    .await
                });
            }
            while let Some(preparation) = preparation_tasks.join_next().await {
                let preparation = preparation.map_err(|error| ScannerError::Io {
                    path: root_path.clone(),
                    source: std::io::Error::other(error.to_string()),
                })?;
                prepared_files.record(preparation);
            }
            root_identity_lost = prepared_files.root_identity_lost;

            if !root_identity_lost {
                let root_still_matches =
                    stat_manifest_root(root_path.clone())
                        .await
                        .is_ok_and(|observed| {
                            manifest_root_identity_matches(
                                expected_root_device,
                                expected_root_inode,
                                &observed,
                            )
                        });
                root_identity_lost = !root_still_matches;
            }

            if !root_identity_lost {
                let mut removal_outcomes = Vec::with_capacity(removal_candidates.len());
                for delta in &removal_candidates {
                    let outcome = match stat_manifest_relative_file(
                        root_path.clone(),
                        delta.relative_path.clone(),
                        expected_root_device,
                        expected_root_inode,
                    )
                    .await
                    {
                        Ok(None) => ManifestRemovalOutcome::Missing,
                        Ok(Some(_)) => ManifestRemovalOutcome::Present,
                        Err(ScannerError::RootIdentityChanged(_)) => {
                            root_identity_lost = true;
                            ManifestRemovalOutcome::RootIdentityChanged
                        }
                        Err(ScannerError::Io { .. }) => ManifestRemovalOutcome::PathIoError,
                        Err(_) => ManifestRemovalOutcome::InvalidPath,
                    };
                    removal_outcomes.push((delta.id.clone(), outcome));
                    if root_identity_lost {
                        break;
                    }
                }
                if !root_identity_lost {
                    let root_still_matches_after_removal_probes =
                        stat_manifest_root(root_path.clone())
                            .await
                            .is_ok_and(|observed| {
                                manifest_root_identity_matches(
                                    expected_root_device,
                                    expected_root_inode,
                                    &observed,
                                )
                            });
                    root_identity_lost = !root_still_matches_after_removal_probes;
                }
                let decision = classify_manifest_removal_outcomes(&removal_outcomes);
                root_identity_lost |= decision.root_identity_lost;
                prepared_files
                    .unstable_delta_ids
                    .extend(decision.unstable_ids);
                if !root_identity_lost {
                    for delta in &removal_candidates {
                        if !decision
                            .confirmed_missing_ids
                            .iter()
                            .any(|id| id == &delta.id)
                        {
                            continue;
                        }
                        if is_supported_movie_file(Path::new(&delta.relative_path)) {
                            removed_media_paths.push(delta.relative_path.clone());
                        }
                        if is_supported_sidecar_file(Path::new(&delta.relative_path)) {
                            removed_sidecar_paths.push(delta.relative_path.clone());
                        }
                    }
                }
            }

            if root_identity_lost {
                self.database
                    .mark_scan_manifest_root_unavailable(manifest_id, &root_id)
                    .await?;
                prepared_files
                    .unstable_delta_ids
                    .extend(root_deltas.iter().map(|delta| delta.id.clone()));
                prepared_files.movie_files.clear();
                prepared_files.episode_files.clear();
                prepared_files.unresolved_files.clear();
                prepared_files.sidecar_entries.clear();
                removed_media_paths.clear();
                removed_sidecar_paths.clear();
            }
            prepared_files.unstable_delta_ids.sort();
            prepared_files.unstable_delta_ids.dedup();

            let transaction_started = Instant::now();
            let result = self
                .database
                .commit_scan_manifest_delta_batch(&ManifestDeltaBatchCommit {
                    job_id: &job.id,
                    manifest_id,
                    library_id: &job.library_id,
                    library_root_id: &root_id,
                    generation: &job.generation,
                    deltas: &root_deltas,
                    unstable_delta_ids: &prepared_files.unstable_delta_ids,
                    movie_files: &prepared_files.movie_files,
                    episode_files: &prepared_files.episode_files,
                    unresolved_files: &prepared_files.unresolved_files,
                    sidecar_entries: &prepared_files.sidecar_entries,
                    removed_media_paths: &removed_media_paths,
                    removed_sidecar_paths: &removed_sidecar_paths,
                })
                .await?;
            transaction_elapsed += transaction_started.elapsed();
            processed = processed.saturating_add(
                result
                    .applied_count
                    .saturating_add(result.conflict_count)
                    .saturating_add(result.unstable_count),
            );
            created_items = created_items.saturating_add(result.created_items);
            if result.metadata_targets_changed {
                self.notify_local_metadata_worker(&job.id);
            }
        }

        if tracing::enabled!(target: "lux::scan_performance", tracing::Level::DEBUG) {
            let elapsed = apply_started.elapsed();
            tracing::debug!(
                target: "lux::scan_performance",
                application_ms = elapsed.saturating_sub(transaction_elapsed).as_millis() as u64,
                transaction_ms = transaction_elapsed.as_millis() as u64,
                delta_count = deltas.len() as u64,
                "manifest apply phase timing"
            );
        }
        if self
            .cancellation_requested(&job.id, false, cancellation)
            .await?
        {
            return self.cancel_running_job(&job.id).await;
        }
        if self
            .database
            .list_pending_scan_manifest_deltas(manifest_id, 1)
            .await?
            .is_empty()
        {
            let mut report = self.finish_scan_manifest_indexing(job, manifest_id).await?;
            report.processed = report.processed.saturating_add(processed);
            report.created_items = report.created_items.saturating_add(created_items);
            return Ok(report);
        }
        Ok(ScanBatchReport {
            status: "RUNNING".to_owned(),
            processed,
            created_items,
            completed: false,
        })
    }

    pub async fn materialize_manifest_postprocessing_targets(
        &self,
        job_id: &str,
    ) -> Result<bool, ScanJobError> {
        let _scan_permit = self.acquire_scan_lock_for_job(job_id).await?;
        self.materialize_manifest_postprocessing_targets_unlocked(job_id)
            .await
    }

    async fn materialize_manifest_postprocessing_targets_unlocked(
        &self,
        job_id: &str,
    ) -> Result<bool, ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Ok(false);
        };
        let Some(manifest) = self.database.get_scan_manifest_by_job(job_id).await? else {
            return Ok(false);
        };
        if !matches!(manifest.workflow_version, 2 | 3) || manifest.discovery_format_version != 3 {
            return Ok(false);
        }
        if job.status != "COMPLETED" || job.scan_phase != "POSTPROCESSING" {
            return Err(StorageError::Conflict(
                "manifest targets can only materialize during postprocessing".to_owned(),
            )
            .into());
        }

        let mut targets_changed = false;
        loop {
            let current_manifest = self
                .database
                .get_scan_manifest(&manifest.id)
                .await?
                .ok_or_else(|| StorageError::Conflict("scan manifest disappeared".to_owned()))?;
            if current_manifest.postprocessing_targets_ready {
                return Ok(targets_changed);
            }
            let roots = self
                .database
                .list_scan_manifest_postprocessing_roots(&manifest.id)
                .await?;
            let pending_roots = roots
                .iter()
                .filter(|root| root.target_stage != "DONE")
                .collect::<Vec<_>>();
            if pending_roots.is_empty() {
                if self
                    .database
                    .finish_empty_scan_manifest_postprocessing_targets(&manifest.id)
                    .await?
                {
                    return Ok(targets_changed);
                }
                continue;
            }

            for root in pending_roots {
                if root.has_stage_rows {
                    let root_path = PathBuf::from(&root.canonical_path);
                    let root_matches =
                        stat_manifest_root(root_path.clone())
                            .await
                            .is_ok_and(|observed| {
                                manifest_root_identity_matches(
                                    root.expected_device,
                                    root.expected_inode,
                                    &observed,
                                )
                            });
                    if !root_matches {
                        self.database
                            .mark_scan_manifest_root_unavailable(
                                &manifest.id,
                                &root.library_root_id,
                            )
                            .await?;
                        self.record_event(
                            job_id,
                            "WARN",
                            "POSTPROCESSING_TARGET_ROOT_UNAVAILABLE",
                            "Manifest target 物化期间根目录身份发生变化，已保留游标等待恢复",
                            "{}",
                        )
                        .await;
                        return Err(ScannerError::RootIdentityChanged(root_path).into());
                    }
                    self.database
                        .update_library_root_availability(&root.library_root_id, true)
                        .await?;
                }
                let result = self
                    .database
                    .materialize_scan_manifest_postprocessing_target_page(
                        ManifestPostprocessingTargetPage {
                            job_id,
                            manifest_id: &manifest.id,
                            library_root_id: &root.library_root_id,
                            generation: &job.generation,
                            target_stage: &root.target_stage,
                            target_cursor: root.target_cursor.as_deref(),
                            page_size: MANIFEST_POSTPROCESSING_TARGET_PAGE_SIZE,
                        },
                    )
                    .await?;
                targets_changed |= result.targets_changed;
                if result.targets_ready {
                    return Ok(targets_changed);
                }
            }
        }
    }

    async fn finish_scan_manifest_indexing(
        &self,
        job: &StoredScanJob,
        manifest_id: &str,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let manifest = self
            .database
            .get_scan_manifest(manifest_id)
            .await?
            .ok_or_else(|| StorageError::Conflict("scan manifest disappeared".to_owned()))?;
        if manifest.state == "APPLYING"
            && !self
                .database
                .transition_scan_manifest_state(manifest_id, "APPLYING", "INDEXED")
                .await?
        {
            return Err(StorageError::Conflict(
                "manifest could not enter indexed state".to_owned(),
            )
            .into());
        }
        let roots = self.database.list_library_roots(&job.library_id).await?;
        for root in roots {
            self.database
                .update_root_scan_cursor(&root.id, None)
                .await?;
        }
        self.database
            .update_library_last_scan(&job.library_id)
            .await?;
        self.database
            .clear_reconciliation_scan_entries(&job.id)
            .await?;
        self.database.mark_scan_job_postprocessing(&job.id).await?;
        let transitioned = self
            .database
            .transition_scan_manifest_state(manifest_id, "INDEXED", "POSTPROCESSING")
            .await?;
        if !transitioned && manifest.state != "POSTPROCESSING" {
            return Err(StorageError::Conflict(
                "manifest could not enter postprocessing state".to_owned(),
            )
            .into());
        }
        self.record_event(&job.id, "INFO", "JOB_COMPLETED", "任务已完成", "{}")
            .await;
        let completed_job = self.database.find_scan_job(&job.id).await?;
        if let Some(completed_job) = completed_job.as_ref() {
            let removed_count = self
                .database
                .count_applied_scan_manifest_removals(manifest_id)
                .await?;
            if removed_count > 0 {
                self.publish_webhook_event_with_data(
                    completed_job,
                    WebhookEventType::MediaRemoved,
                    None,
                    json!({ "removedCount": removed_count }),
                )
                .await;
            }
            self.publish_webhook_event(completed_job, WebhookEventType::ScanCompleted, None)
                .await;
        }
        self.clear_cancellation_flag(&job.id);
        if let Some(home) = &self.home {
            home.invalidate_scan_batch().await;
        }
        self.flush_home_after_scan_terminal().await;
        Ok(ScanBatchReport {
            status: "COMPLETED".to_owned(),
            processed: 0,
            created_items: 0,
            completed: true,
        })
    }

    async fn discover_reconciliation_directory_batches(
        &self,
        job_id: &str,
        root: &StoredLibraryRoot,
        relative_directory: &str,
        cancellation: &AtomicBool,
    ) -> Result<Option<usize>, ScannerError> {
        let relative = Path::new(relative_directory);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(ScannerError::InvalidRelativePath(
                relative_directory.to_owned(),
            ));
        }
        let root_path = Path::new(&root.canonical_path);
        let directory_path = root_path.join(relative);
        let mut entries =
            fs::read_dir(&directory_path)
                .await
                .map_err(|source| ScannerError::Io {
                    path: directory_path.clone(),
                    source,
                })?;
        let mut directories = Vec::with_capacity(DISCOVERY_ENTRY_BATCH_SIZE);
        let mut media_files = Vec::with_capacity(DISCOVERY_ENTRY_BATCH_SIZE);
        let mut inserted_media_files = 0_usize;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| ScannerError::Io {
                path: directory_path.clone(),
                source,
            })?
        {
            if cancellation.load(Ordering::Acquire) {
                return Ok(None);
            }
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|source| ScannerError::Io {
                path: path.clone(),
                source,
            })?;
            if !(file_type.is_dir()
                || file_type.is_file()
                    && (is_supported_movie_file(&path) || is_supported_sidecar_file(&path)))
            {
                continue;
            }
            let relative_path = path
                .strip_prefix(root_path)
                .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                .to_str()
                .ok_or(ScannerError::NonUtf8Path)?
                .to_owned();
            if file_type.is_dir() {
                directories.push(relative_path);
            } else {
                media_files.push(relative_path);
            }
            if directories.len() >= DISCOVERY_ENTRY_BATCH_SIZE
                || media_files.len() >= DISCOVERY_ENTRY_BATCH_SIZE
            {
                directories.sort_unstable();
                media_files.sort_unstable();
                let inserted_count = self
                    .database
                    .commit_reconciliation_discovery_chunk(
                        job_id,
                        &root.id,
                        &directories,
                        &media_files,
                        None,
                    )
                    .await?;
                inserted_media_files = inserted_media_files
                    .saturating_add(usize::try_from(inserted_count).unwrap_or(usize::MAX));
                directories.clear();
                media_files.clear();
            }
        }
        directories.sort_unstable();
        media_files.sort_unstable();
        let inserted_count = self
            .database
            .commit_reconciliation_discovery_chunk(
                job_id,
                &root.id,
                &directories,
                &media_files,
                Some(relative_directory),
            )
            .await?;
        inserted_media_files = inserted_media_files
            .saturating_add(usize::try_from(inserted_count).unwrap_or(usize::MAX));
        Ok(Some(inserted_media_files))
    }

    async fn run_reconciliation_discovery_batch(
        &self,
        job: &StoredScanJob,
        batch_size: usize,
        cancellation: &AtomicBool,
        stream_files_during_discovery: bool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let limit = i64::try_from(batch_size.min(DISCOVERY_BATCH_SIZE)).unwrap_or(i64::MAX);
        let directories = self
            .database
            .list_reconciliation_scan_entries(&job.id, "DIRECTORY", limit)
            .await?;
        self.update_activity(
            &job.id,
            directories
                .last()
                .map(|directory| directory.relative_path.as_str()),
            "DISCOVERY",
        )
        .await?;
        let root_ids = directories
            .iter()
            .map(|directory| directory.library_root_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let roots_by_id = self.database.list_library_roots_by_ids(&root_ids).await?;
        let mut unavailable_root_ids = HashSet::new();
        let mut discovered_count = job.total_count;
        for directory in directories {
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(&job.id).await;
            }
            if unavailable_root_ids.contains(&directory.library_root_id) {
                continue;
            }
            let Some(root) = roots_by_id.get(&directory.library_root_id).cloned() else {
                self.database
                    .discard_reconciliation_root_entries(&job.id, &directory.library_root_id)
                    .await?;
                continue;
            };
            match self
                .discover_reconciliation_directory_batches(
                    &job.id,
                    &root,
                    &directory.relative_path,
                    cancellation,
                )
                .await
            {
                Ok(Some(discovered_count_for_directory)) => {
                    discovered_count = discovered_count.saturating_add(
                        i64::try_from(discovered_count_for_directory).unwrap_or(i64::MAX),
                    );
                    if !root.is_available {
                        self.database
                            .update_library_root_availability(&root.id, true)
                            .await?;
                    }
                }
                Ok(None) => return self.cancel_running_job(&job.id).await,
                Err(ScannerError::Io { .. }) => {
                    unavailable_root_ids.insert(root.id.clone());
                    self.database
                        .update_library_root_availability(&root.id, false)
                        .await?;
                    self.database
                        .discard_reconciliation_root_entries(&job.id, &root.id)
                        .await?;
                    self.record_event(
                        &job.id,
                        "WARN",
                        "ROOT_UNAVAILABLE",
                        "媒体库根路径不可用，已跳过本轮缺失判定",
                        "{}",
                    )
                    .await;
                }
                Err(error) => {
                    return self
                        .fail_reconciliation_job(job, error, &[], job.processed_count)
                        .await;
                }
            }
        }

        let remaining = self
            .database
            .list_reconciliation_scan_entries(&job.id, "DIRECTORY", 1)
            .await?;
        if remaining.is_empty() {
            let total = self
                .database
                .finish_reconciliation_discovery(&job.id)
                .await?;
            let details = format!(r#"{{"discovered":{total},"discoveryCompleted":true}}"#);
            self.record_event(
                &job.id,
                "INFO",
                "DISCOVERY_COMPLETED",
                "媒体库目录发现完成",
                &details,
            )
            .await;
        } else {
            let details =
                format!(r#"{{"discovered":{discovered_count},"discoveryCompleted":false}}"#);
            self.record_event(
                &job.id,
                "INFO",
                "DISCOVERY_PROGRESS",
                "媒体库目录发现进行中",
                &details,
            )
            .await;
        }
        if stream_files_during_discovery {
            return self
                .run_reconciliation_file_batch(job, batch_size, cancellation, remaining.is_empty())
                .await;
        }
        Ok(ScanBatchReport {
            status: "RUNNING".to_owned(),
            processed: 0,
            created_items: 0,
            completed: false,
        })
    }

    async fn run_reconciliation_file_batch(
        &self,
        job: &StoredScanJob,
        batch_size: usize,
        cancellation: &AtomicBool,
        discovery_completed: bool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let roots = self.database.list_library_roots(&job.library_id).await?;
        let library = self.database.find_library(&job.library_id).await?;
        let library_kind = library
            .as_ref()
            .map_or("MOVIE", |library| library.kind.as_str());
        let scan_concurrency = configured_scan_concurrency(
            self.scan_concurrency_override,
            library.as_ref().map(|library| library.scan_concurrency),
            self.default_scan_concurrency,
        );
        let batch = self
            .database
            .list_reconciliation_scan_entries(
                &job.id,
                "FILE",
                i64::try_from(batch_size).unwrap_or(i64::MAX),
            )
            .await?;
        self.update_activity(
            &job.id,
            batch.last().map(|entry| entry.relative_path.as_str()),
            if batch.is_empty() {
                if discovery_completed {
                    "FINALIZING"
                } else {
                    "DISCOVERY"
                }
            } else {
                "INDEXING"
            },
        )
        .await?;
        if batch.is_empty() {
            if !discovery_completed {
                return Ok(ScanBatchReport {
                    status: "RUNNING".to_owned(),
                    processed: 0,
                    created_items: 0,
                    completed: false,
                });
            }
            let mut removed_count = 0_usize;
            for root in &roots {
                if !root.is_available {
                    continue;
                }
                let root_is_available = fs::metadata(&root.canonical_path)
                    .await
                    .is_ok_and(|metadata| metadata.is_dir());
                if !root_is_available {
                    self.database
                        .update_library_root_availability(&root.id, false)
                        .await?;
                    continue;
                }
                let mut after_relative_path = None;
                loop {
                    let missing_paths = self
                        .database
                        .list_reconciliation_missing_filesystem_entry_paths_page(
                            &job.id,
                            &root.id,
                            after_relative_path.as_deref(),
                            i64::try_from(MISSING_ENTRY_BATCH_SIZE).unwrap_or(i64::MAX),
                        )
                        .await?;
                    let Some(last_path) = missing_paths.last().cloned() else {
                        break;
                    };
                    let mut confirmed_missing_paths = Vec::with_capacity(missing_paths.len());
                    for relative_path in &missing_paths {
                        let path = Path::new(&root.canonical_path).join(relative_path);
                        match fs::metadata(&path).await {
                            Ok(metadata) if metadata.is_file() => {}
                            Ok(_) => confirmed_missing_paths.push(relative_path.clone()),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                confirmed_missing_paths.push(relative_path.clone());
                            }
                            Err(_) => {
                                // An inaccessible path is unknown, not proof that the media
                                // was deleted. Leave it for a later reconciliation attempt.
                            }
                        }
                    }
                    let removed_media_paths = confirmed_missing_paths
                        .iter()
                        .filter(|path| is_supported_movie_file(Path::new(path)))
                        .cloned()
                        .collect::<Vec<_>>();
                    let removed_sidecar_paths = confirmed_missing_paths
                        .iter()
                        .filter(|path| is_supported_sidecar_file(Path::new(path)))
                        .cloned()
                        .collect::<Vec<_>>();
                    if !confirmed_missing_paths.is_empty() {
                        removed_count = removed_count.saturating_add(
                            usize::try_from(
                                self.database
                                    .finalize_reconciliation_root_page(
                                        &job.id,
                                        &root.id,
                                        &job.generation,
                                        &confirmed_missing_paths,
                                        &removed_media_paths,
                                        &removed_sidecar_paths,
                                    )
                                    .await?,
                            )
                            .unwrap_or(usize::MAX),
                        );
                    }
                    after_relative_path = Some(last_path);
                }
                self.database.refresh_removed_media_items(&root.id).await?;
                self.database
                    .update_root_scan_cursor(&root.id, None)
                    .await?;
            }
            self.database
                .update_library_last_scan(&job.library_id)
                .await?;
            self.database
                .update_scan_job_progress(&job.id, None, job.processed_count)
                .await?;
            self.database
                .clear_reconciliation_scan_entries(&job.id)
                .await?;
            self.database.mark_scan_job_postprocessing(&job.id).await?;
            self.record_event(&job.id, "INFO", "JOB_COMPLETED", "任务已完成", "{}")
                .await;
            let completed_job = self.database.find_scan_job(&job.id).await?;
            if let Some(completed_job) = completed_job.as_ref() {
                if removed_count > 0 {
                    self.publish_webhook_event_with_data(
                        completed_job,
                        WebhookEventType::MediaRemoved,
                        None,
                        json!({ "removedCount": removed_count }),
                    )
                    .await;
                }
                self.publish_webhook_event(completed_job, WebhookEventType::ScanCompleted, None)
                    .await;
            }
            return Ok(ScanBatchReport {
                status: "COMPLETED".to_owned(),
                processed: 0,
                created_items: 0,
                completed: true,
            });
        }

        if library_kind == "MOVIE" {
            return self
                .run_movie_reconciliation_file_batch(
                    job,
                    &roots,
                    &batch,
                    scan_concurrency,
                    cancellation,
                )
                .await;
        }

        let mut processed = 0_usize;
        let mut next_count = job.processed_count;
        let mut completed_entries = Vec::<StoredReconciliationScanEntry>::new();
        let mut created_items = 0_usize;
        let mut existing_entries_by_root =
            HashMap::<String, HashMap<String, StoredFilesystemEntry>>::new();
        let mut batch_paths_by_root = HashMap::<String, Vec<String>>::new();
        let mut changed_paths_by_root = HashMap::<String, Vec<String>>::new();
        let mut new_paths_by_root = HashMap::<String, Vec<String>>::new();
        let mut changed_sidecar_paths_by_root = HashMap::<String, Vec<String>>::new();
        let mut new_works_by_root = HashMap::<String, Vec<ReconciliationScanWork>>::new();
        let mut root_has_existing_index = HashMap::<String, bool>::new();
        let mut quick_seen_entry_ids = HashMap::<String, Vec<String>>::new();
        let mut missing_paths_by_root = HashMap::<String, Vec<String>>::new();
        let mut regular_works = Vec::<ReconciliationRegularWork>::new();
        let mut classification_cache = MixedClassificationCache::default();
        for entry in &batch {
            batch_paths_by_root
                .entry(entry.library_root_id.clone())
                .or_default()
                .push(entry.relative_path.clone());
        }
        for (root_id, paths) in batch_paths_by_root {
            let existing_entries = self
                .database
                .list_filesystem_entries_for_paths(&root_id, &paths)
                .await?;
            existing_entries_by_root.insert(root_id, existing_entries);
        }
        let concurrency = self.effective_scan_concurrency(scan_concurrency).await;
        let mut quick_results = self
            .scan_reconciliation_file_fingerprints(
                &roots,
                &batch,
                &existing_entries_by_root,
                concurrency,
            )
            .await?;
        for (entry_index, entry) in batch.iter().enumerate() {
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(&job.id).await;
            }
            let Some(root) = roots.iter().find(|root| root.id == entry.library_root_id) else {
                self.database
                    .discard_reconciliation_root_entries(&job.id, &entry.library_root_id)
                    .await?;
                continue;
            };
            if !root.is_available {
                regular_works.retain(|work| work.root.id != root.id);
                new_works_by_root.remove(&root.id);
                self.database
                    .update_library_root_availability(&root.id, false)
                    .await?;
                let discarded = self
                    .database
                    .discard_reconciliation_root_entries(&job.id, &root.id)
                    .await?;
                next_count = next_count.saturating_add(discarded);
                processed =
                    processed.saturating_add(usize::try_from(discarded).unwrap_or(usize::MAX));
                self.database
                    .update_scan_job_progress(&job.id, None, next_count)
                    .await?;
                continue;
            }
            let path = Path::new(&root.canonical_path).join(&entry.relative_path);
            if fs::metadata(&path).await.is_err() {
                let root_is_available = fs::metadata(&root.canonical_path)
                    .await
                    .is_ok_and(|metadata| metadata.is_dir());
                if !root_is_available {
                    regular_works.retain(|work| work.root.id != root.id);
                    new_works_by_root.remove(&root.id);
                    self.database
                        .update_library_root_availability(&root.id, false)
                        .await?;
                    let already_processed = completed_entries
                        .iter()
                        .filter(|completed| completed.library_root_id == root.id)
                        .count();
                    let discarded = self
                        .database
                        .discard_reconciliation_root_entries(&job.id, &root.id)
                        .await?
                        .saturating_sub(i64::try_from(already_processed).unwrap_or(i64::MAX));
                    next_count = next_count.saturating_add(discarded);
                    processed =
                        processed.saturating_add(usize::try_from(discarded).unwrap_or(usize::MAX));
                    self.database
                        .update_scan_job_progress(&job.id, None, next_count)
                        .await?;
                    continue;
                }
                next_count = next_count.saturating_add(1);
                processed = processed.saturating_add(1);
                missing_paths_by_root
                    .entry(root.id.clone())
                    .or_default()
                    .push(entry.relative_path.clone());
                completed_entries.push(entry.clone());
                continue;
            }

            if !is_supported_movie_file(&path) {
                let existing_entries = existing_entries_by_root
                    .get(&root.id)
                    .ok_or_else(|| ScannerError::LibraryNotFound)?;
                let (entry_id, changed) = self
                    .scanner
                    .scan_sidecar_file(
                        root,
                        Path::new(&root.canonical_path),
                        &path,
                        existing_entries,
                        &job.generation,
                    )
                    .await?;
                let target_needs_recovery = existing_entries
                    .get(&entry.relative_path)
                    .is_some_and(|existing| existing.last_seen_generation == job.generation);
                if changed || target_needs_recovery {
                    changed_sidecar_paths_by_root
                        .entry(root.id.clone())
                        .or_default()
                        .push(entry.relative_path.clone());
                    // The target is persisted by the atomic root batch before
                    // the reconciliation entry is acknowledged. If file
                    // preparation fails first, the pending entry and the
                    // generation marker let the retry recover the target.
                } else {
                    quick_seen_entry_ids
                        .entry(root.id.clone())
                        .or_default()
                        .push(entry_id);
                }
                next_count = next_count.saturating_add(1);
                processed = processed.saturating_add(1);
                completed_entries.push(entry.clone());
                continue;
            }

            let existing_entries = existing_entries_by_root
                .get(&root.id)
                .ok_or_else(|| ScannerError::LibraryNotFound)?;
            if let Some((entry_id, quick_report)) = quick_results[entry_index].take() {
                quick_seen_entry_ids
                    .entry(root.id.clone())
                    .or_default()
                    .push(entry_id);
                if existing_entries
                    .get(&entry.relative_path)
                    .is_some_and(|existing| existing.last_seen_generation == job.generation)
                {
                    changed_paths_by_root
                        .entry(root.id.clone())
                        .or_default()
                        .push(entry.relative_path.clone());
                }
                next_count = next_count.saturating_add(1);
                processed = processed.saturating_add(1);
                created_items = created_items.saturating_add(quick_report.created_items);
                completed_entries.push(entry.clone());
                continue;
            }
            let classification = match library_kind {
                "SERIES" => MixedClassification::Episode,
                "HOMEVIDEOS" => MixedClassification::Unresolved,
                "MIXED" => {
                    classify_mixed_file(
                        Path::new(&root.canonical_path),
                        &path,
                        &mut classification_cache,
                    )
                    .await
                }
                _ => return Err(ScanJobError::LibraryNotFound),
            };
            let is_new = !existing_entries.contains_key(&entry.relative_path);
            let batchable = is_new
                && matches!(
                    classification,
                    MixedClassification::Movie | MixedClassification::Episode
                );
            let root_has_index = if batchable {
                if let Some(has_index) = root_has_existing_index.get(&root.id) {
                    *has_index
                } else {
                    let has_index = self
                        .database
                        .has_filesystem_entries_for_root(&root.id)
                        .await?;
                    root_has_existing_index.insert(root.id.clone(), has_index);
                    has_index
                }
            } else {
                false
            };
            let requires_compatibility_scan = batchable
                && root_has_index
                && (self
                    .scanner
                    .file_has_moved_entry(
                        &job.library_id,
                        root,
                        Path::new(&root.canonical_path),
                        &path,
                    )
                    .await?
                    || (matches!(classification, MixedClassification::Episode)
                        && self
                            .scanner
                            .episode_path_has_legacy_identity(
                                root,
                                Path::new(&root.canonical_path),
                                &path,
                            )
                            .await?));
            if batchable && !requires_compatibility_scan {
                new_paths_by_root
                    .entry(root.id.clone())
                    .or_default()
                    .push(entry.relative_path.clone());
                new_works_by_root.entry(root.id.clone()).or_default().push(
                    ReconciliationScanWork {
                        entry: entry.clone(),
                        path,
                        classification,
                    },
                );
                continue;
            }
            let target_paths = if existing_entries.contains_key(&entry.relative_path) {
                &mut changed_paths_by_root
            } else {
                &mut new_paths_by_root
            };
            target_paths
                .entry(root.id.clone())
                .or_default()
                .push(entry.relative_path.clone());

            regular_works.push(ReconciliationRegularWork {
                index: regular_works.len(),
                entry: entry.clone(),
                root: root.clone(),
                path,
                classification,
            });
        }
        let mut regular_tasks: JoinSet<ReconciliationRegularTask> = JoinSet::new();
        let mut regular_results = (0..regular_works.len()).map(|_| None).collect::<Vec<_>>();
        for (index, work) in regular_works.iter().enumerate() {
            if cancellation.load(Ordering::Acquire) {
                regular_tasks.abort_all();
                return self.cancel_running_job(&job.id).await;
            }
            while regular_tasks.len() >= concurrency {
                if let Err(error) =
                    collect_reconciliation_regular_task(&mut regular_tasks, &mut regular_results)
                        .await
                {
                    return self
                        .fail_reconciliation_job(job, error, &completed_entries, next_count)
                        .await;
                }
            }
            let scanner = self.scanner.clone();
            let library_id = job.library_id.clone();
            let generation = job.generation.clone();
            let root = work.root.clone();
            let path = work.path.clone();
            let classification = work.classification;
            let is_home_videos = library_kind == "HOMEVIDEOS";
            regular_tasks.spawn(async move {
                let report = if is_home_videos {
                    scanner
                        .scan_home_video_file(
                            &library_id,
                            &root,
                            Path::new(&root.canonical_path),
                            &path,
                            &generation,
                        )
                        .await?
                } else {
                    match classification {
                        MixedClassification::Movie => {
                            scanner
                                .scan_movie_file(
                                    &library_id,
                                    &root,
                                    Path::new(&root.canonical_path),
                                    &path,
                                    &generation,
                                )
                                .await?
                        }
                        MixedClassification::Episode => {
                            scanner
                                .scan_episode_file(
                                    &library_id,
                                    &root,
                                    Path::new(&root.canonical_path),
                                    &path,
                                    &generation,
                                )
                                .await?
                        }
                        MixedClassification::Unresolved => {
                            scanner
                                .scan_unresolved_file(
                                    &library_id,
                                    &root,
                                    Path::new(&root.canonical_path),
                                    &path,
                                    &generation,
                                )
                                .await?
                        }
                    }
                };
                Ok((index, report))
            });
        }
        while !regular_tasks.is_empty() {
            if let Err(error) =
                collect_reconciliation_regular_task(&mut regular_tasks, &mut regular_results).await
            {
                return self
                    .fail_reconciliation_job(job, error, &completed_entries, next_count)
                    .await;
            }
        }
        if cancellation.load(Ordering::Acquire) {
            return self.cancel_running_job(&job.id).await;
        }
        for (work, report) in regular_works
            .into_iter()
            .zip(regular_results.into_iter().flatten())
        {
            created_items = created_items.saturating_add(report.created_items);
            next_count = next_count.saturating_add(1);
            processed = processed.saturating_add(1);
            completed_entries.push(work.entry);
        }
        let mut prepared_movie_files = HashMap::<String, Vec<NewMovieFile>>::new();
        let mut prepared_episode_files = HashMap::<String, Vec<NewEpisodeFile>>::new();
        for (root_id, works) in new_works_by_root {
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(&job.id).await;
            }
            let mut movie_works = Vec::new();
            let mut episode_works = Vec::new();
            for work in works {
                match work.classification {
                    MixedClassification::Movie => movie_works.push(work),
                    MixedClassification::Episode => episode_works.push(work),
                    MixedClassification::Unresolved => {}
                }
            }
            let Some(root) = roots.iter().find(|root| root.id == root_id) else {
                continue;
            };
            if !movie_works.is_empty() {
                let paths = movie_works
                    .iter()
                    .map(|work| work.path.clone())
                    .collect::<Vec<_>>();
                let prepared = match self
                    .scanner
                    .prepare_new_movie_files_with_concurrency(
                        Path::new(&root.canonical_path),
                        &paths,
                        concurrency,
                    )
                    .await
                {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        return self
                            .fail_reconciliation_job(job, error, &completed_entries, next_count)
                            .await;
                    }
                };
                for (work, prepared) in movie_works.into_iter().zip(prepared) {
                    if let Some(file) = prepared {
                        prepared_movie_files
                            .entry(root_id.clone())
                            .or_default()
                            .push(file);
                    }
                    next_count = next_count.saturating_add(1);
                    processed = processed.saturating_add(1);
                    completed_entries.push(work.entry);
                }
            }
            if !episode_works.is_empty() {
                let paths = episode_works
                    .iter()
                    .map(|work| work.path.clone())
                    .collect::<Vec<_>>();
                let prepared = match self
                    .scanner
                    .prepare_new_episode_files_with_concurrency(
                        root,
                        Path::new(&root.canonical_path),
                        &paths,
                        concurrency,
                    )
                    .await
                {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        return self
                            .fail_reconciliation_job(job, error, &completed_entries, next_count)
                            .await;
                    }
                };
                for (work, prepared) in episode_works.into_iter().zip(prepared) {
                    if let Some(file) = prepared {
                        prepared_episode_files
                            .entry(root_id.clone())
                            .or_default()
                            .push(file);
                    }
                    next_count = next_count.saturating_add(1);
                    processed = processed.saturating_add(1);
                    completed_entries.push(work.entry);
                }
            }
        }
        if cancellation.load(Ordering::Acquire) {
            return self.cancel_running_job(&job.id).await;
        }
        for root in roots {
            let root_entries = completed_entries
                .iter()
                .filter(|entry| entry.library_root_id == root.id)
                .cloned()
                .collect::<Vec<_>>();
            let batch = ReconciliationBatchCommit {
                job_id: &job.id,
                library_id: &job.library_id,
                library_root_id: &root.id,
                generation: &job.generation,
                entries: &root_entries,
                movie_files: prepared_movie_files
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                episode_files: prepared_episode_files
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                seen_entry_ids: quick_seen_entry_ids
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                missing_paths: missing_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                new_paths: new_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                changed_paths: changed_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                sidecar_paths: changed_sidecar_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
            };
            let (confirmed, inserted) = self.commit_reconciliation_root_batch(&batch).await?;
            processed = processed.saturating_sub(root_entries.len());
            processed = processed.saturating_add(confirmed);
            created_items = created_items.saturating_add(inserted);
        }
        self.record_reconciliation_batch_event(
            job,
            processed,
            created_items,
            next_count,
            concurrency,
        )
        .await
    }

    async fn scan_reconciliation_file_fingerprints(
        &self,
        roots: &[StoredLibraryRoot],
        batch: &[StoredReconciliationScanEntry],
        existing_entries_by_root: &HashMap<String, HashMap<String, StoredFilesystemEntry>>,
        concurrency: usize,
    ) -> Result<Vec<Option<(String, ScanReport)>>, ScannerError> {
        let mut results = (0..batch.len()).map(|_| None).collect::<Vec<_>>();
        let mut tasks: JoinSet<ReconciliationFingerprintTask> = JoinSet::new();
        for (index, entry) in batch.iter().enumerate() {
            let Some(root) = roots.iter().find(|root| root.id == entry.library_root_id) else {
                continue;
            };
            let path = Path::new(&root.canonical_path).join(&entry.relative_path);
            if !is_supported_movie_file(&path) {
                continue;
            }
            let Some(existing_entry) = existing_entries_by_root
                .get(&root.id)
                .and_then(|entries| entries.get(&entry.relative_path))
                .cloned()
            else {
                continue;
            };
            while tasks.len() >= concurrency.max(1) {
                collect_reconciliation_fingerprint_task(&mut tasks, &mut results).await?;
            }
            let scanner = self.scanner.clone();
            let root_path = PathBuf::from(&root.canonical_path);
            tasks.spawn(async move {
                let result = match scanner
                    .scan_file_if_fingerprint_unchanged(&root_path, &path, &existing_entry)
                    .await
                {
                    Ok(result) => result,
                    Err(ScannerError::Io { .. }) => None,
                    Err(error) => return Err(error),
                };
                Ok((index, result))
            });
        }
        while !tasks.is_empty() {
            collect_reconciliation_fingerprint_task(&mut tasks, &mut results).await?;
        }
        Ok(results)
    }

    async fn run_movie_reconciliation_file_batch(
        &self,
        job: &StoredScanJob,
        roots: &[StoredLibraryRoot],
        batch: &[StoredReconciliationScanEntry],
        configured_concurrency: i64,
        cancellation: &AtomicBool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let mut processed = 0_usize;
        let mut next_count = job.processed_count;
        let mut created_items = 0_usize;
        let mut completed_entries = Vec::<(usize, StoredReconciliationScanEntry)>::new();
        let mut unavailable_root_ids = HashSet::<String>::new();
        let mut existing_entries_by_root =
            HashMap::<String, HashMap<String, StoredFilesystemEntry>>::new();
        let mut batch_paths_by_root = HashMap::<String, Vec<String>>::new();
        let mut changed_paths_by_root = HashMap::<String, Vec<String>>::new();
        let mut new_paths_by_root = HashMap::<String, Vec<String>>::new();
        let mut changed_sidecar_paths_by_root = HashMap::<String, Vec<String>>::new();
        let mut quick_seen_entry_ids = HashMap::<String, Vec<String>>::new();
        let mut missing_paths_by_root = HashMap::<String, Vec<String>>::new();
        let mut new_files = Vec::<(
            usize,
            String,
            PathBuf,
            PathBuf,
            StoredReconciliationScanEntry,
        )>::new();
        let mut regular_works = Vec::<ReconciliationRegularWork>::new();

        for entry in batch {
            if roots.iter().any(|root| root.id == entry.library_root_id) {
                batch_paths_by_root
                    .entry(entry.library_root_id.clone())
                    .or_default()
                    .push(entry.relative_path.clone());
            }
        }
        for (root_id, paths) in batch_paths_by_root {
            let existing_entries = self
                .database
                .list_filesystem_entries_for_paths(&root_id, &paths)
                .await?;
            existing_entries_by_root.insert(root_id, existing_entries);
        }
        let concurrency = self
            .effective_scan_concurrency(configured_concurrency)
            .await;
        let mut quick_results = self
            .scan_reconciliation_file_fingerprints(
                roots,
                batch,
                &existing_entries_by_root,
                concurrency,
            )
            .await?;

        for (index, entry) in batch.iter().enumerate() {
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(&job.id).await;
            }
            if unavailable_root_ids.contains(&entry.library_root_id) {
                continue;
            }
            let Some(root) = roots.iter().find(|root| root.id == entry.library_root_id) else {
                self.database
                    .discard_reconciliation_root_entries(&job.id, &entry.library_root_id)
                    .await?;
                unavailable_root_ids.insert(entry.library_root_id.clone());
                continue;
            };
            if !root.is_available {
                unavailable_root_ids.insert(root.id.clone());
                self.database
                    .update_library_root_availability(&root.id, false)
                    .await?;
                let discarded = self
                    .database
                    .discard_reconciliation_root_entries(&job.id, &root.id)
                    .await?;
                next_count = next_count.saturating_add(discarded);
                processed =
                    processed.saturating_add(usize::try_from(discarded).unwrap_or(usize::MAX));
                self.database
                    .update_scan_job_progress(&job.id, None, next_count)
                    .await?;
                continue;
            }

            let path = Path::new(&root.canonical_path).join(&entry.relative_path);
            if fs::metadata(&path).await.is_err() {
                let root_is_available = fs::metadata(&root.canonical_path)
                    .await
                    .is_ok_and(|metadata| metadata.is_dir());
                if !root_is_available {
                    unavailable_root_ids.insert(root.id.clone());
                    self.database
                        .update_library_root_availability(&root.id, false)
                        .await?;
                    let already_processed = completed_entries
                        .iter()
                        .filter(|(_, completed)| completed.library_root_id == root.id)
                        .count();
                    let discarded = self
                        .database
                        .discard_reconciliation_root_entries(&job.id, &root.id)
                        .await?
                        .saturating_sub(i64::try_from(already_processed).unwrap_or(i64::MAX));
                    next_count = next_count.saturating_add(discarded);
                    processed =
                        processed.saturating_add(usize::try_from(discarded).unwrap_or(usize::MAX));
                    self.database
                        .update_scan_job_progress(&job.id, None, next_count)
                        .await?;
                    continue;
                }
                next_count = next_count.saturating_add(1);
                processed = processed.saturating_add(1);
                missing_paths_by_root
                    .entry(root.id.clone())
                    .or_default()
                    .push(entry.relative_path.clone());
                completed_entries.push((index, entry.clone()));
                continue;
            }

            if !is_supported_movie_file(&path) {
                let existing_entries = existing_entries_by_root
                    .get(&root.id)
                    .ok_or_else(|| ScannerError::LibraryNotFound)?;
                let (entry_id, changed) = self
                    .scanner
                    .scan_sidecar_file(
                        root,
                        Path::new(&root.canonical_path),
                        &path,
                        existing_entries,
                        &job.generation,
                    )
                    .await?;
                let target_needs_recovery = existing_entries
                    .get(&entry.relative_path)
                    .is_some_and(|existing| existing.last_seen_generation == job.generation);
                if changed || target_needs_recovery {
                    changed_sidecar_paths_by_root
                        .entry(root.id.clone())
                        .or_default()
                        .push(entry.relative_path.clone());
                } else {
                    quick_seen_entry_ids
                        .entry(root.id.clone())
                        .or_default()
                        .push(entry_id);
                }
                next_count = next_count.saturating_add(1);
                processed = processed.saturating_add(1);
                completed_entries.push((index, entry.clone()));
                continue;
            }

            let existing_entries = existing_entries_by_root
                .get(&root.id)
                .ok_or_else(|| ScannerError::LibraryNotFound)?;
            if let Some((entry_id, quick_report)) = quick_results[index].take() {
                quick_seen_entry_ids
                    .entry(root.id.clone())
                    .or_default()
                    .push(entry_id);
                if existing_entries
                    .get(&entry.relative_path)
                    .is_some_and(|existing| existing.last_seen_generation == job.generation)
                {
                    changed_paths_by_root
                        .entry(root.id.clone())
                        .or_default()
                        .push(entry.relative_path.clone());
                }
                next_count = next_count.saturating_add(1);
                processed = processed.saturating_add(1);
                completed_entries.push((index, entry.clone()));
                created_items = created_items.saturating_add(quick_report.created_items);
                continue;
            }
            if existing_entries.contains_key(&entry.relative_path) {
                changed_paths_by_root
                    .entry(root.id.clone())
                    .or_default()
                    .push(entry.relative_path.clone());
                regular_works.push(ReconciliationRegularWork {
                    index,
                    entry: entry.clone(),
                    root: root.clone(),
                    path,
                    classification: MixedClassification::Movie,
                });
                continue;
            }

            new_paths_by_root
                .entry(root.id.clone())
                .or_default()
                .push(entry.relative_path.clone());
            new_files.push((
                index,
                root.id.clone(),
                root.canonical_path.clone().into(),
                path,
                entry.clone(),
            ));
        }

        let mut grouped_regular_works =
            Vec::<Vec<(usize, ReconciliationRegularWork, Option<String>)>>::new();
        let mut regular_group_indexes = HashMap::<String, usize>::new();
        for (regular_index, work) in regular_works.iter().cloned().enumerate() {
            let inferred_suffix = if matches!(work.classification, MixedClassification::Movie) {
                infer_sibling_movie_variant_suffix(&work.path).await
            } else {
                None
            };
            let group_key = reconciliation_regular_group_key(
                &work.root.id,
                &work.path,
                work.classification,
                inferred_suffix.as_deref(),
                work.index,
            );
            let group_index = *regular_group_indexes.entry(group_key).or_insert_with(|| {
                grouped_regular_works.push(Vec::new());
                grouped_regular_works.len() - 1
            });
            grouped_regular_works[group_index].push((regular_index, work, inferred_suffix));
        }
        let mut regular_tasks: JoinSet<ReconciliationRegularGroupTask> = JoinSet::new();
        let mut regular_results = (0..regular_works.len()).map(|_| None).collect::<Vec<_>>();
        for group in grouped_regular_works {
            if cancellation.load(Ordering::Acquire) {
                regular_tasks.abort_all();
                return self.cancel_running_job(&job.id).await;
            }
            while regular_tasks.len() >= concurrency {
                if let Err(error) = collect_reconciliation_regular_group_task(
                    &mut regular_tasks,
                    &mut regular_results,
                )
                .await
                {
                    let completed = completed_entries
                        .iter()
                        .map(|(_, entry)| entry.clone())
                        .collect::<Vec<_>>();
                    return self
                        .fail_reconciliation_job(job, error, &completed, next_count)
                        .await;
                }
            }
            let scanner = self.scanner.clone();
            let library_id = job.library_id.clone();
            let generation = job.generation.clone();
            regular_tasks.spawn(async move {
                let mut reports = Vec::with_capacity(group.len());
                for (regular_index, work, inferred_suffix) in group {
                    let root = work.root;
                    let path = work.path;
                    let report = match work.classification {
                        MixedClassification::Movie => {
                            scanner
                                .scan_movie_file_with_inferred_suffix(
                                    &library_id,
                                    &root,
                                    Path::new(&root.canonical_path),
                                    &path,
                                    &generation,
                                    inferred_suffix.as_deref(),
                                )
                                .await?
                        }
                        MixedClassification::Episode => {
                            scanner
                                .scan_episode_file(
                                    &library_id,
                                    &root,
                                    Path::new(&root.canonical_path),
                                    &path,
                                    &generation,
                                )
                                .await?
                        }
                        MixedClassification::Unresolved => {
                            scanner
                                .scan_unresolved_file(
                                    &library_id,
                                    &root,
                                    Path::new(&root.canonical_path),
                                    &path,
                                    &generation,
                                )
                                .await?
                        }
                    };
                    reports.push((regular_index, report));
                }
                Ok(reports)
            });
        }
        while !regular_tasks.is_empty() {
            if let Err(error) =
                collect_reconciliation_regular_group_task(&mut regular_tasks, &mut regular_results)
                    .await
            {
                let completed = completed_entries
                    .iter()
                    .map(|(_, entry)| entry.clone())
                    .collect::<Vec<_>>();
                return self
                    .fail_reconciliation_job(job, error, &completed, next_count)
                    .await;
            }
        }
        if cancellation.load(Ordering::Acquire) {
            return self.cancel_running_job(&job.id).await;
        }
        for (work, report) in regular_works
            .into_iter()
            .zip(regular_results.into_iter().flatten())
        {
            created_items = created_items.saturating_add(report.created_items);
            next_count = next_count.saturating_add(1);
            processed = processed.saturating_add(1);
            completed_entries.push((work.index, work.entry));
        }

        let mut preparation_tasks: JoinSet<MoviePreparationTask> = JoinSet::new();
        let mut active_tasks = 0_usize;
        let mut prepared_files = HashMap::<String, Vec<NewMovieFile>>::new();
        for (index, root_id, root_path, path, entry) in new_files {
            if cancellation.load(Ordering::Acquire) {
                preparation_tasks.abort_all();
                return self.cancel_running_job(&job.id).await;
            }
            if active_tasks >= concurrency {
                let prepared = join_movie_preparation(&mut preparation_tasks).await;
                let (index, root_id, entry, file) = match prepared {
                    Ok(result) => result,
                    Err(error) => {
                        let completed = completed_entries
                            .iter()
                            .map(|(_, entry)| entry.clone())
                            .collect::<Vec<_>>();
                        return self
                            .fail_reconciliation_job(job, error, &completed, next_count)
                            .await;
                    }
                };
                active_tasks = active_tasks.saturating_sub(1);
                if cancellation.load(Ordering::Acquire) {
                    preparation_tasks.abort_all();
                    return self.cancel_running_job(&job.id).await;
                }
                if let Some(file) = file {
                    prepared_files.entry(root_id).or_default().push(file);
                }
                next_count = next_count.saturating_add(1);
                processed = processed.saturating_add(1);
                completed_entries.push((index, entry));
            }
            let scanner = self.scanner.clone();
            preparation_tasks.spawn(async move {
                let prepared = scanner.prepare_new_movie_file(&root_path, &path).await?;
                Ok((index, root_id, entry, prepared))
            });
            active_tasks = active_tasks.saturating_add(1);
        }
        while active_tasks > 0 {
            let prepared = join_movie_preparation(&mut preparation_tasks).await;
            let (index, root_id, entry, file) = match prepared {
                Ok(result) => result,
                Err(error) => {
                    let completed = completed_entries
                        .iter()
                        .map(|(_, entry)| entry.clone())
                        .collect::<Vec<_>>();
                    return self
                        .fail_reconciliation_job(job, error, &completed, next_count)
                        .await;
                }
            };
            active_tasks = active_tasks.saturating_sub(1);
            if cancellation.load(Ordering::Acquire) {
                preparation_tasks.abort_all();
                return self.cancel_running_job(&job.id).await;
            }
            if let Some(file) = file {
                prepared_files.entry(root_id).or_default().push(file);
            }
            next_count = next_count.saturating_add(1);
            processed = processed.saturating_add(1);
            completed_entries.push((index, entry));
        }

        completed_entries.sort_by_key(|(index, _)| *index);
        let completed_entries = completed_entries
            .into_iter()
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>();
        for root in roots {
            let root_entries = completed_entries
                .iter()
                .filter(|entry| entry.library_root_id == root.id)
                .cloned()
                .collect::<Vec<_>>();
            let batch = ReconciliationBatchCommit {
                job_id: &job.id,
                library_id: &job.library_id,
                library_root_id: &root.id,
                generation: &job.generation,
                entries: &root_entries,
                movie_files: prepared_files.get(&root.id).map_or(&[][..], Vec::as_slice),
                episode_files: &[],
                seen_entry_ids: quick_seen_entry_ids
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                missing_paths: missing_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                new_paths: new_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                changed_paths: changed_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                sidecar_paths: changed_sidecar_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
            };
            let (confirmed, inserted) = self.commit_reconciliation_root_batch(&batch).await?;
            processed = processed.saturating_sub(root_entries.len());
            processed = processed.saturating_add(confirmed);
            created_items = created_items.saturating_add(inserted);
        }
        self.record_reconciliation_batch_event(
            job,
            processed,
            created_items,
            next_count,
            concurrency,
        )
        .await
    }

    async fn commit_reconciliation_root_batch(
        &self,
        batch: &ReconciliationBatchCommit<'_>,
    ) -> Result<(usize, usize), ScanJobError> {
        let result = self.database.commit_reconciliation_batch(batch).await?;
        if result.metadata_targets_changed {
            self.notify_local_metadata_worker(batch.job_id);
        }
        Ok((result.confirmed_entries, result.created_items))
    }

    async fn record_reconciliation_batch_event(
        &self,
        job: &StoredScanJob,
        processed: usize,
        created_items: usize,
        next_count: i64,
        concurrency: usize,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let batch_details = format!(
            r#"{{"processed":{processed},"total":{next_count},"concurrency":{concurrency}}}"#
        );
        self.record_event(
            &job.id,
            "INFO",
            "BATCH_COMPLETED",
            "扫描批次完成",
            &batch_details,
        )
        .await;
        if self.database.scan_job_cancel_requested(&job.id).await? {
            let mut cancelled = self.cancel_running_job(&job.id).await?;
            cancelled.processed = processed;
            cancelled.created_items = created_items;
            return Ok(cancelled);
        }
        Ok(ScanBatchReport {
            status: "RUNNING".to_owned(),
            processed,
            created_items,
            completed: false,
        })
    }

    async fn fail_reconciliation_job(
        &self,
        job: &StoredScanJob,
        error: ScannerError,
        _completed_entries: &[StoredReconciliationScanEntry],
        _next_count: i64,
    ) -> Result<ScanBatchReport, ScanJobError> {
        if scanner_error_sqlite_lock_code(&error).is_some() {
            return Err(error.into());
        }
        let error_code = error.code();
        self.database
            .finish_scan_manifest(&job.id, "FAILED")
            .await?;
        if let Some(manifest) = self.database.get_scan_manifest_by_job(&job.id).await? {
            self.clear_lite_manifest_discovery_session(&manifest.id);
        }
        self.database
            .finish_scan_job(&job.id, "FAILED", Some(&error.to_string()))
            .await?;
        if job.scan_phase != "POSTPROCESSING"
            && let Some(home) = &self.home
        {
            home.invalidate_scan_batch().await;
        }
        self.record_event(&job.id, "ERROR", error_code, "扫描任务失败", "{}")
            .await;
        let failed_job = self.database.find_scan_job(&job.id).await?;
        self.publish_webhook_event(
            failed_job.as_ref().unwrap_or(job),
            WebhookEventType::ScanFailed,
            Some(error_code),
        )
        .await;
        self.clear_cancellation_flag(&job.id);
        Err(error.into())
    }

    async fn fail_unhandled_scan_job(
        &self,
        job_id: &str,
        error: &ScanJobError,
    ) -> Result<(), ScanJobError> {
        if matches!(error, ScanJobError::AlreadyActive(_)) {
            return Ok(());
        }
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Ok(());
        };
        if !matches!(job.status.as_str(), "PENDING" | "RUNNING") {
            return Ok(());
        }
        let error_code = error.code();
        self.database.finish_scan_manifest(job_id, "FAILED").await?;
        if let Some(manifest) = self.database.get_scan_manifest_by_job(job_id).await? {
            self.clear_lite_manifest_discovery_session(&manifest.id);
        }
        self.database
            .finish_scan_job(job_id, "FAILED", Some(&error.to_string()))
            .await?;
        if job.scan_phase != "POSTPROCESSING"
            && let Some(home) = &self.home
        {
            home.invalidate_scan_batch().await;
        }
        self.record_event(job_id, "ERROR", error_code, "扫描任务失败", "{}")
            .await;
        let failed_job = self.database.find_scan_job(job_id).await?;
        self.publish_webhook_event(
            failed_job.as_ref().unwrap_or(&job),
            WebhookEventType::ScanFailed,
            Some(error_code),
        )
        .await;
        self.clear_cancellation_flag(job_id);
        Ok(())
    }

    async fn verify_manifest_postprocessing_target_roots(
        &self,
        job_id: &str,
    ) -> Result<(), ScanJobError> {
        let Some(manifest) = self
            .database
            .get_scan_manifest_postprocessing_state_by_job(job_id)
            .await?
        else {
            return Ok(());
        };
        if !matches!(manifest.workflow_version, 2 | 3) || manifest.discovery_format_version != 3 {
            return Ok(());
        }
        for root in manifest.roots {
            if !root.has_positive_rows {
                continue;
            }
            let root_path = PathBuf::from(&root.canonical_path);
            let root_matches = stat_manifest_root(root_path.clone())
                .await
                .is_ok_and(|observed| {
                    manifest_root_identity_matches(
                        root.expected_device,
                        root.expected_inode,
                        &observed,
                    )
                });
            if !root_matches {
                self.database
                    .mark_scan_manifest_root_unavailable(
                        &manifest.manifest_id,
                        &root.library_root_id,
                    )
                    .await?;
                self.record_event(
                    job_id,
                    "WARN",
                    "POSTPROCESSING_TARGET_ROOT_UNAVAILABLE",
                    "后处理目标根目录身份发生变化，已保留目标供恢复后重试",
                    "{}",
                )
                .await;
                return Err(ScannerError::RootIdentityChanged(root_path).into());
            }
            self.database
                .update_library_root_availability(&root.library_root_id, true)
                .await?;
        }
        Ok(())
    }

    async fn run_incremental_batch(
        &self,
        job_id: &str,
        batch_size: usize,
        cancellation: &AtomicBool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Err(ScanJobError::JobNotFound);
        };
        if matches!(job.status.as_str(), "COMPLETED" | "CANCELLED" | "FAILED") {
            self.clear_cancellation_flag(job_id);
            return Ok(ScanBatchReport {
                status: job.status,
                processed: 0,
                created_items: 0,
                completed: true,
            });
        }
        if job.status == "PENDING" {
            if !self.database.claim_scan_job(job_id).await? {
                return Err(ScanJobError::AlreadyActive(job_id.to_owned()));
            }
            self.record_event(job_id, "INFO", "JOB_STARTED", "局部扫描任务开始执行", "{}")
                .await;
        }
        if self
            .cancellation_requested(job_id, job.cancel_requested, cancellation)
            .await?
        {
            return self.cancel_running_job(job_id).await;
        }
        let paths = self
            .database
            .list_pending_scan_job_paths(job_id, i64::try_from(batch_size).unwrap_or(i64::MAX))
            .await?;
        self.update_activity(
            job_id,
            paths.last().map(|path| path.relative_path.as_str()),
            if paths.is_empty() {
                "FINALIZING"
            } else {
                "INDEXING"
            },
        )
        .await?;
        if paths.is_empty() {
            if self.database.finish_scan_job_if_idle(job_id).await? {
                self.database
                    .update_library_last_scan(&job.library_id)
                    .await?;
                self.record_event(job_id, "INFO", "JOB_COMPLETED", "局部扫描任务已完成", "{}")
                    .await;
                let completed_job = self.database.find_scan_job(job_id).await?;
                self.publish_webhook_event(
                    completed_job.as_ref().unwrap_or(&job),
                    WebhookEventType::ScanCompleted,
                    None,
                )
                .await;
                self.clear_cancellation_flag(job_id);
                return Ok(ScanBatchReport {
                    status: "COMPLETED".to_owned(),
                    processed: 0,
                    created_items: 0,
                    completed: true,
                });
            }
            return Ok(ScanBatchReport {
                status: "RUNNING".to_owned(),
                processed: 0,
                created_items: 0,
                completed: false,
            });
        }
        let library = self
            .database
            .find_library(&job.library_id)
            .await?
            .ok_or(ScanJobError::LibraryNotFound)?;
        let roots = self.database.list_library_roots(&job.library_id).await?;
        let roots_by_id = roots
            .into_iter()
            .map(|root| (root.id.clone(), root))
            .collect::<HashMap<_, _>>();
        let mut created_items = 0_usize;
        for path in &paths {
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(job_id).await;
            }
            let created = match self
                .process_incremental_path(
                    &library.kind,
                    &job,
                    path,
                    roots_by_id.get(&path.library_root_id),
                    cancellation,
                )
                .await
            {
                Ok(created) => created,
                Err(error) => {
                    self.database
                        .finish_scan_job(job_id, "FAILED", Some(&error.to_string()))
                        .await?;
                    self.record_event(job_id, "ERROR", error.code(), "局部扫描任务失败", "{}")
                        .await;
                    let failed_job = self.database.find_scan_job(job_id).await?;
                    self.publish_webhook_event(
                        failed_job.as_ref().unwrap_or(&job),
                        WebhookEventType::ScanFailed,
                        Some(error.code()),
                    )
                    .await;
                    return Err(error.into());
                }
            };
            created_items = created_items.saturating_add(created);
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(job_id).await;
            }
            self.database
                .mark_scan_job_path_processed(job_id, &path.library_root_id, &path.relative_path)
                .await?;
        }
        let processed = paths.len();
        let next_count = job
            .processed_count
            .saturating_add(i64::try_from(processed).unwrap_or(i64::MAX));
        self.database
            .update_scan_job_progress(
                job_id,
                paths.last().map(|path| path.relative_path.as_str()),
                next_count,
            )
            .await?;
        self.record_event(job_id, "INFO", "BATCH_COMPLETED", "局部扫描批次完成", "{}")
            .await;
        Ok(ScanBatchReport {
            status: "RUNNING".to_owned(),
            processed,
            created_items,
            completed: false,
        })
    }

    async fn publish_webhook_event(
        &self,
        job: &StoredScanJob,
        event_type: WebhookEventType,
        error_code: Option<&str>,
    ) {
        self.publish_webhook_event_with_data(job, event_type, error_code, json!({}))
            .await;
    }

    async fn publish_media_added_event(&self, job: &StoredScanJob, added_count: usize) {
        if added_count > 0 {
            self.publish_webhook_event_with_data(
                job,
                WebhookEventType::MediaAdded,
                None,
                json!({ "addedCount": added_count }),
            )
            .await;
        }
    }

    async fn publish_webhook_event_with_data(
        &self,
        job: &StoredScanJob,
        event_type: WebhookEventType,
        error_code: Option<&str>,
        extra: Value,
    ) {
        let Some(webhooks) = self.webhooks.as_ref() else {
            return;
        };
        let occurred_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or(0);
        let dedupe_key = format!("scan:{}:{}", job.id, event_type.as_str());
        let library_name = self
            .database
            .find_library(&job.library_id)
            .await
            .ok()
            .flatten()
            .map(|library| library.name);
        let duration_seconds = job
            .started_at
            .map(|started_at| occurred_at.saturating_sub(started_at));
        let mut data = json!({
            "jobId": job.id,
            "libraryId": job.library_id,
            "libraryName": library_name,
            "jobType": job.job_type,
            "status": job.status,
            "processedCount": job.processed_count,
            "totalCount": job.total_count,
            "durationSeconds": duration_seconds,
            "errorCode": error_code,
        });
        if let (Value::Object(data), Value::Object(extra)) = (&mut data, extra) {
            data.extend(extra);
        }
        let result = webhooks
            .publish(event_type, &dedupe_key, occurred_at, data)
            .await;
        if result.is_err() {
            tracing::warn!(job_id = %job.id, event_type = event_type.as_str(), "failed to enqueue webhook event");
        }
    }

    async fn process_incremental_path(
        &self,
        library_kind: &str,
        job: &StoredScanJob,
        path: &StoredScanJobPath,
        root: Option<&StoredLibraryRoot>,
        cancellation: &AtomicBool,
    ) -> Result<usize, ScannerError> {
        let root = root.ok_or(ScannerError::LibraryNotFound)?;
        let root_path = Path::new(&root.canonical_path);
        let media_path = root_path.join(&path.relative_path);
        let mut classification_cache = MixedClassificationCache::default();
        let metadata = if path.change_kind == "REMOVE" {
            None
        } else {
            fs::metadata(&media_path).await.ok()
        };
        if metadata.is_none() {
            if is_supported_sidecar_file(&media_path) {
                let sidecar_paths = [path.relative_path.clone()];
                let targets_changed = self
                    .database
                    .record_scan_job_sidecar_targets(&job.id, &root.id, &sidecar_paths)
                    .await?;
                if targets_changed {
                    self.notify_local_metadata_worker(&job.id);
                }
            }
            self.database
                .mark_filesystem_entry_missing_by_path(&root.id, &path.relative_path)
                .await?;
            return Ok(0);
        }
        let Some(metadata) = metadata else {
            return Ok(0);
        };
        if metadata.is_dir() {
            let mut created_items = 0_usize;
            let mut walker = FileBatchWalker::new(&media_path);
            while let Some(files) = walker.next_batch(FILE_BATCH_SIZE).await? {
                for file in files {
                    if cancellation.load(Ordering::Acquire) {
                        return Ok(created_items);
                    }
                    if is_supported_sidecar_file(&file) {
                        self.process_incremental_sidecar_file(job, root, root_path, &file)
                            .await?;
                    } else {
                        created_items = created_items.saturating_add(
                            self.process_incremental_file(
                                library_kind,
                                job,
                                root,
                                root_path,
                                &file,
                                &mut classification_cache,
                            )
                            .await?,
                        );
                    }
                }
            }
            Ok(created_items)
        } else if is_supported_movie_file(&media_path) {
            if cancellation.load(Ordering::Acquire) {
                return Ok(0);
            }
            self.process_incremental_file(
                library_kind,
                job,
                root,
                root_path,
                &media_path,
                &mut classification_cache,
            )
            .await
        } else if is_supported_sidecar_file(&media_path) {
            self.process_incremental_sidecar_file(job, root, root_path, &media_path)
                .await?;
            Ok(0)
        } else {
            Ok(0)
        }
    }

    async fn process_incremental_file(
        &self,
        library_kind: &str,
        job: &StoredScanJob,
        root: &StoredLibraryRoot,
        root_path: &Path,
        file: &Path,
        classification_cache: &mut MixedClassificationCache,
    ) -> Result<usize, ScannerError> {
        if is_supported_sidecar_file(file) {
            self.process_incremental_sidecar_file(job, root, root_path, file)
                .await?;
            return Ok(0);
        }
        let generation = &job.generation;
        let report = match library_kind {
            "MOVIE" => {
                self.scanner
                    .scan_movie_file(&job.library_id, root, root_path, file, generation)
                    .await?
            }
            "SERIES" => {
                self.scanner
                    .scan_episode_file(&job.library_id, root, root_path, file, generation)
                    .await?
            }
            "HOMEVIDEOS" => {
                self.scanner
                    .scan_home_video_file(&job.library_id, root, root_path, file, generation)
                    .await?
            }
            "MIXED" => match classify_mixed_file(root_path, file, classification_cache).await {
                MixedClassification::Movie => {
                    self.scanner
                        .scan_movie_file(&job.library_id, root, root_path, file, generation)
                        .await?
                }
                MixedClassification::Episode => {
                    self.scanner
                        .scan_episode_file(&job.library_id, root, root_path, file, generation)
                        .await?
                }
                MixedClassification::Unresolved => {
                    self.scanner
                        .scan_unresolved_file(&job.library_id, root, root_path, file, generation)
                        .await?
                }
            },
            _ => return Err(ScannerError::LibraryNotFound),
        };
        let relative_path = file
            .strip_prefix(root_path)
            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
            .to_str()
            .ok_or(ScannerError::NonUtf8Path)?
            .to_owned();
        if report.skipped_files == 0
            || report.changed_files > 0
            || report.created_items > 0
            || report.created_sources > 0
        {
            let targets_changed = self
                .database
                .record_scan_job_targets(&job.id, &root.id, &[relative_path], "CHANGED")
                .await?;
            if targets_changed {
                self.notify_local_metadata_worker(&job.id);
            }
        }
        Ok(report.created_items)
    }

    async fn process_incremental_sidecar_file(
        &self,
        job: &StoredScanJob,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
    ) -> Result<(), ScannerError> {
        let relative_path = path
            .strip_prefix(root_path)
            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
            .to_str()
            .ok_or(ScannerError::NonUtf8Path)?
            .to_owned();
        let relative_paths = [relative_path.clone()];
        let existing_entries = self
            .database
            .list_filesystem_entries_for_paths(&root.id, &relative_paths)
            .await?;
        let (_, changed) = self
            .scanner
            .scan_sidecar_file(root, root_path, path, &existing_entries, &job.generation)
            .await?;
        if changed {
            let targets_changed = self
                .database
                .record_scan_job_sidecar_targets(&job.id, &root.id, &relative_paths)
                .await?;
            if targets_changed {
                self.notify_local_metadata_worker(&job.id);
            }
        }
        Ok(())
    }

    pub async fn start_local_metadata_outbox_worker(&self) -> Result<(), ScanJobError> {
        if self
            .local_metadata_outbox_worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Ok(());
        }
        if let Err(error) = self
            .database
            .requeue_interrupted_scan_local_metadata_batches()
            .await
        {
            self.local_metadata_outbox_worker_started
                .store(false, Ordering::Release);
            return Err(error.into());
        }
        if let Err(error) = self
            .database
            .requeue_interrupted_scan_local_metadata_backfills()
            .await
        {
            self.local_metadata_outbox_worker_started
                .store(false, Ordering::Release);
            return Err(error.into());
        }
        if let Err(error) = self
            .database
            .ensure_scan_local_metadata_backfill_roots()
            .await
        {
            self.local_metadata_outbox_worker_started
                .store(false, Ordering::Release);
            return Err(error.into());
        }

        let database = self.database.clone();
        let local_nfo = self.local_nfo.clone();
        let metadata_selection = self.metadata_selection.clone();
        let metadata_reidentify = self.metadata_reidentify.clone();
        let home = self.home.clone();
        let user_events = self.user_events.clone();
        let resources = self.resources.clone();
        let outbox_notify = Arc::clone(&self.local_metadata_outbox_notify);
        let image_notifications = Arc::clone(&self.local_metadata_image_notifications);
        tokio::spawn(async move {
            let enricher = MetadataEnricher::new(database.clone()).with_resource_metrics(resources);
            let enricher = match local_nfo {
                Some(local_nfo) => enricher.with_nfo_store(local_nfo),
                None => enricher,
            };
            let mut nfo_tasks = JoinSet::new();
            loop {
                if nfo_tasks.len() >= MAX_SCAN_LOCAL_METADATA_NFO_BATCHES_IN_FLIGHT {
                    if let Some(Err(error)) = nfo_tasks.join_next().await {
                        tracing::error!(%error, "local metadata NFO worker task panicked");
                    }
                    continue;
                }
                match database.claim_next_scan_local_metadata_batch().await {
                    Ok(Some(batch)) => {
                        if let Some((
                            batch_id,
                            scan_job_id,
                            source_snapshot,
                            non_retryable_item_ids,
                        )) = process_scan_local_metadata_batch(
                            &database,
                            &enricher,
                            home.as_ref(),
                            &user_events,
                            batch,
                        )
                        .await
                        {
                            notify_local_metadata_images_waiter(&image_notifications, &scan_job_id);
                            let database = database.clone();
                            let enricher = enricher.clone();
                            let metadata_selection = metadata_selection.clone();
                            let metadata_reidentify = metadata_reidentify.clone();
                            let home = home.clone();
                            let user_events = user_events.clone();
                            nfo_tasks.spawn(async move {
                                let context = LocalMetadataCompletionContext {
                                    metadata_selection: metadata_selection.as_ref(),
                                    metadata_reidentify: metadata_reidentify.as_ref(),
                                    home: home.as_ref(),
                                    user_events: &user_events,
                                };
                                finish_scan_local_metadata_batch(
                                    &database,
                                    &enricher,
                                    context,
                                    &batch_id,
                                    &scan_job_id,
                                    source_snapshot,
                                    &non_retryable_item_ids,
                                )
                                .await;
                                batch_id
                            });
                        }
                    }
                    Ok(None) => {
                        match database
                            .claim_next_scan_local_metadata_backfill_page(
                                SCAN_LOCAL_METADATA_BACKFILL_PAGE_SIZE,
                            )
                            .await
                        {
                            Ok(Some(page)) => {
                                match process_scan_local_metadata_backfill_images(
                                    &enricher,
                                    home.as_ref(),
                                    &user_events,
                                    &page,
                                )
                                .await
                                {
                                    Ok(source_snapshot) => {
                                        let database = database.clone();
                                        let enricher = enricher.clone();
                                        let metadata_selection = metadata_selection.clone();
                                        let metadata_reidentify = metadata_reidentify.clone();
                                        let home = home.clone();
                                        let user_events = user_events.clone();
                                        let library_root_id = page.library_root_id.clone();
                                        nfo_tasks.spawn(async move {
                                            let context = LocalMetadataCompletionContext {
                                                metadata_selection: metadata_selection.as_ref(),
                                                metadata_reidentify: metadata_reidentify.as_ref(),
                                                home: home.as_ref(),
                                                user_events: &user_events,
                                            };
                                            finish_scan_local_metadata_backfill_page(
                                                &database,
                                                &enricher,
                                                context,
                                                page,
                                                source_snapshot,
                                            )
                                            .await;
                                            library_root_id
                                        });
                                    }
                                    Err(error) => {
                                        tracing::warn!(
                                            library_root_id = %page.library_root_id,
                                            %error,
                                            "local metadata backfill image stage failed and will be retried"
                                        );
                                        fail_scan_local_metadata_backfill_page(
                                            &database,
                                            &page,
                                            &error,
                                            &[],
                                        )
                                        .await;
                                    }
                                }
                            }
                            Ok(None) => {
                                if nfo_tasks.is_empty() {
                                    tokio::select! {
                                        _ = outbox_notify.notified() => {}
                                        _ = tokio::time::sleep(SCAN_LOCAL_METADATA_OUTBOX_IDLE_FALLBACK) => {}
                                    }
                                } else {
                                    tokio::select! {
                                        result = nfo_tasks.join_next() => {
                                            if let Some(Err(error)) = result {
                                                tracing::error!(%error, "local metadata worker task panicked");
                                            }
                                        }
                                        _ = outbox_notify.notified() => {}
                                        _ = tokio::time::sleep(SCAN_LOCAL_METADATA_OUTBOX_IDLE_FALLBACK) => {}
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, "local metadata backfill claim failed; retrying");
                                tokio::time::sleep(SCAN_LOCAL_METADATA_OUTBOX_IDLE_FALLBACK).await;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "local metadata outbox claim failed; retrying");
                        tokio::time::sleep(SCAN_LOCAL_METADATA_OUTBOX_IDLE_FALLBACK).await;
                    }
                }
            }
        });
        Ok(())
    }

    fn notify_local_metadata_outbox_worker(&self) {
        self.local_metadata_outbox_notify.notify_one();
    }

    fn start_local_metadata_worker(&self, scan_job_id: &str) -> LocalMetadataWorkerHandle {
        let (stop, mut stop_receiver) = watch::channel(false);
        #[cfg(test)]
        let (idle_page_sender, idle_page_receiver) = tokio::sync::mpsc::unbounded_channel();
        let database = self.database.clone();
        let people = self.people.clone();
        let local_nfo = self.local_nfo.clone();
        let metadata_selection = self.metadata_selection.clone();
        let metadata_reidentify = self.metadata_reidentify.clone();
        let home = self.home.clone();
        let user_events = self.user_events.clone();
        let resources = self.resources.clone();
        let target_root_validator = self.clone();
        let scan_job_id = scan_job_id.to_owned();
        let notifications = Arc::clone(&self.metadata_notifications);
        let notify = Arc::new(Notify::new());
        if let Ok(mut registry) = notifications.lock() {
            registry.insert(scan_job_id.clone(), Arc::clone(&notify));
        }
        let worker_job_id = scan_job_id.clone();
        let task = tokio::spawn(async move {
            let enricher = MetadataEnricher::new(database.clone()).with_resource_metrics(resources);
            let enricher = match people {
                Some(people) => enricher.with_people(people),
                None => enricher,
            };
            let enricher = match local_nfo {
                Some(local_nfo) => enricher.with_nfo_store(local_nfo),
                None => enricher,
            };

            let mut loaded_lifecycle = false;
            loop {
                if *stop_receiver.borrow() {
                    return;
                }
                let page = match database
                    .load_scan_job_metadata_page(
                        &worker_job_id,
                        i64::try_from(LOCAL_METADATA_BATCH_SIZE).unwrap_or(i64::MAX),
                    )
                    .await
                {
                    Ok(page) => {
                        loaded_lifecycle = true;
                        page
                    }
                    Err(error) => {
                        tracing::warn!(
                            scan_job_id = %worker_job_id,
                            %error,
                            "local metadata worker could not load job lifecycle and target page"
                        );
                        let fallback = if loaded_lifecycle {
                            LOCAL_METADATA_JOB_REFRESH_FALLBACK
                        } else {
                            LOCAL_METADATA_IDLE_FALLBACK
                        };
                        tokio::select! {
                            changed = stop_receiver.changed() => {
                                if changed.is_err() || *stop_receiver.borrow() {
                                    return;
                                }
                            }
                            _ = notify.notified() => {}
                            _ = tokio::time::sleep(fallback) => {}
                        }
                        continue;
                    }
                };
                let Some(job_status) = page.job_status.as_deref() else {
                    return;
                };
                if matches!(job_status, "FAILED" | "CANCELLED") {
                    return;
                }
                let is_incremental_scan = page.job_type.as_deref() == Some("INCREMENTAL_SCAN");
                let completeness_policy = metadata_auto_match_policy_for_scan_job_snapshot(
                    page.job_type.as_deref(),
                    page.auto_metadata_match,
                );
                if page.has_pending {
                    if let Err(error) = target_root_validator
                        .verify_manifest_postprocessing_target_roots(&worker_job_id)
                        .await
                    {
                        tracing::warn!(
                            scan_job_id = %worker_job_id,
                            %error,
                            "local metadata worker deferred targets because a manifest root changed"
                        );
                        if let Err(mark_error) = database
                            .mark_pending_scan_job_metadata_targets_failed(
                                &worker_job_id,
                                &error.to_string(),
                            )
                            .await
                        {
                            tracing::warn!(
                                scan_job_id = %worker_job_id,
                                %mark_error,
                                "local metadata worker could not defer root-mismatched targets"
                            );
                        }
                        notify.notify_waiters();
                        continue;
                    }
                    match enricher
                        .enrich_scan_job_metadata_page(&worker_job_id, page)
                        .await
                    {
                        Ok(report) if report.items_processed > 0 => {
                            if is_incremental_scan
                                && !report.locally_enriched_item_ids.is_empty()
                                && let Err(error) =
                                    complete_local_metadata_completeness_for_item_ids(
                                        &database,
                                        metadata_selection.as_ref(),
                                        metadata_reidentify.as_ref(),
                                        LocalMetadataCompletenessTrigger::ScanJobPolicy(
                                            completeness_policy,
                                        ),
                                        &report.locally_enriched_item_ids,
                                        &user_events,
                                    )
                                    .await
                            {
                                tracing::warn!(
                                    scan_job_id = %worker_job_id,
                                    %error,
                                    "incremental local metadata completeness could not be saved"
                                );
                            }
                            if let Some(home) = &home {
                                home.invalidate_scan_batch().await;
                            }
                            notify.notify_waiters();
                        }
                        Ok(_) => {
                            // A pending target with no locally readable source
                            // cannot make progress. Mark it retryable so the
                            // completion waiter cannot spin forever; a later
                            // retry can reset FAILED targets after the source
                            // becomes available.
                            if let Err(error) = database
                                .mark_pending_scan_job_metadata_targets_failed(
                                    &worker_job_id,
                                    "local metadata source unavailable",
                                )
                                .await
                            {
                                tracing::warn!(
                                    scan_job_id = %worker_job_id,
                                    %error,
                                    "local metadata worker could not finish unavailable targets"
                                );
                            }
                            notify.notify_waiters();
                        }
                        Err(error) => {
                            tracing::warn!(
                                scan_job_id = %worker_job_id,
                                %error,
                                "local metadata worker failed; pending targets marked for retry"
                            );
                            if let Err(mark_error) = database
                                .mark_pending_scan_job_metadata_targets_failed(
                                    &worker_job_id,
                                    &error.to_string(),
                                )
                                .await
                            {
                                tracing::warn!(
                                    scan_job_id = %worker_job_id,
                                    %mark_error,
                                    "local metadata worker could not mark failed targets"
                                );
                            }
                            notify.notify_waiters();
                        }
                    }
                    continue;
                }
                #[cfg(test)]
                let _ = idle_page_sender.send(());
                let notified = notify.notified();
                tokio::select! {
                    changed = stop_receiver.changed() => {
                        if changed.is_err() || *stop_receiver.borrow() {
                            return;
                        }
                    }
                    _ = notified => {}
                    _ = tokio::time::sleep(LOCAL_METADATA_JOB_REFRESH_FALLBACK) => {}
                }
            }
        });
        LocalMetadataWorkerHandle {
            stop,
            task,
            job_id: scan_job_id,
            notifications,
            #[cfg(test)]
            idle_page_receiver,
        }
    }

    fn notify_local_metadata_worker(&self, scan_job_id: &str) {
        if let Ok(registry) = self.metadata_notifications.lock()
            && let Some(notify) = registry.get(scan_job_id)
        {
            notify.notify_one();
        }
    }

    async fn wait_for_local_metadata(&self, scan_job_id: &str) -> Result<(), ScanJobError> {
        let notify = self
            .metadata_notifications
            .lock()
            .ok()
            .and_then(|registry| registry.get(scan_job_id).cloned());
        loop {
            let notified = notify.as_ref().map(|notify| notify.notified());
            tokio::pin!(notified);
            if let Some(notified) = notified.as_mut().as_pin_mut() {
                notified.enable();
            }

            let (has_pending_targets, job_missing_or_cancelled) = self
                .database
                .local_metadata_completion_wait_state(scan_job_id)
                .await?;
            if !has_pending_targets {
                break;
            }
            let cancellation = self.cancellation_flag(scan_job_id);
            if job_missing_or_cancelled {
                cancellation.store(true, Ordering::Release);
                break;
            }
            if cancellation.load(Ordering::Acquire) {
                break;
            }

            if let Some(notified) = notified.as_mut().as_pin_mut() {
                tokio::select! {
                    _ = notified => {}
                    _ = tokio::time::sleep(LOCAL_METADATA_COMPLETION_FALLBACK) => {}
                }
            } else {
                tokio::time::sleep(LOCAL_METADATA_COMPLETION_FALLBACK).await;
            }
        }
        Ok(())
    }

    async fn wait_for_local_metadata_images(&self, scan_job_id: &str) -> Result<(), ScanJobError> {
        let notify =
            self.local_metadata_image_notifications
                .lock()
                .ok()
                .map(|mut notifications| {
                    Arc::clone(
                        notifications
                            .entry(scan_job_id.to_owned())
                            .or_insert_with(|| Arc::new(Notify::new())),
                    )
                });
        while self
            .database
            .has_pending_scan_local_metadata_images(scan_job_id)
            .await?
        {
            let cancellation = self.cancellation_flag(scan_job_id);
            if self
                .cancellation_requested(scan_job_id, false, &cancellation)
                .await?
            {
                break;
            }
            if let Some(notify) = &notify {
                tokio::select! {
                    _ = notify.notified() => {}
                    _ = tokio::time::sleep(LOCAL_METADATA_IDLE_FALLBACK) => {}
                }
            } else {
                tokio::time::sleep(LOCAL_METADATA_IDLE_FALLBACK).await;
            }
        }
        if let Ok(mut notifications) = self.local_metadata_image_notifications.lock() {
            notifications.remove(scan_job_id);
        }
        Ok(())
    }

    async fn stop_local_metadata_worker(worker: &mut Option<LocalMetadataWorkerHandle>) {
        let Some(worker) = worker.take() else {
            return;
        };
        let _ = worker.stop.send(true);
        let _ = worker.task.await;
        if let Ok(mut notifications) = worker.notifications.lock() {
            notifications.remove(&worker.job_id);
        }
    }

    async fn finish_cancellation_if_requested(
        &self,
        job_id: &str,
        local_metadata_worker: &mut Option<LocalMetadataWorkerHandle>,
    ) -> Result<bool, ScanJobError> {
        let cancellation = self.cancellation_flag(job_id);
        if !self
            .cancellation_requested(job_id, false, &cancellation)
            .await?
        {
            return Ok(false);
        }
        Self::stop_local_metadata_worker(local_metadata_worker).await;
        self.cancel_running_job(job_id).await?;
        Ok(true)
    }

    pub async fn run_to_completion(
        &self,
        job_id: &str,
        batch_size: usize,
        probe: Option<MediaProbeService>,
    ) -> Result<(), ScanJobError> {
        self.run_to_completion_with_metadata_and_thumbnails(job_id, batch_size, probe, None, None)
            .await
    }

    pub async fn run_to_completion_with_metadata(
        &self,
        job_id: &str,
        batch_size: usize,
        probe: Option<MediaProbeService>,
        metadata: Option<MetadataReidentifyService>,
    ) -> Result<(), ScanJobError> {
        self.run_to_completion_with_metadata_and_thumbnails(
            job_id, batch_size, probe, metadata, None,
        )
        .await
    }

    pub async fn run_to_completion_with_metadata_and_thumbnails(
        &self,
        job_id: &str,
        batch_size: usize,
        probe: Option<MediaProbeService>,
        metadata: Option<MetadataReidentifyService>,
        thumbnails: Option<ThumbnailService>,
    ) -> Result<(), ScanJobError> {
        if batch_size == 0 {
            return Err(ScanJobError::InvalidBatchSize);
        }
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            // Library deletion cascades queued jobs. A stale worker wakeup after that
            // cascade has nothing left to run and should finish quietly.
            return Ok(());
        };
        let Some(_run_guard) = self.track_scan_job_run(job_id, &job.library_id) else {
            self.cancel_running_job(job_id).await?;
            return Ok(());
        };
        let _full_scan_permit = if job.job_type == "RECONCILE_LIBRARY" {
            Some(
                Arc::clone(&self.full_scan_queue)
                    .acquire_owned()
                    .await
                    .map_err(|_| ScanJobError::ScanLockClosed)?,
            )
        } else {
            None
        };
        let result = self
            .run_to_completion_with_metadata_and_thumbnails_inner(
                job_id, batch_size, probe, metadata, thumbnails,
            )
            .await;
        if result.is_err() {
            match self.database.fail_scan_job_postprocessing(job_id).await {
                Ok(true) => {
                    self.record_event(
                        job_id,
                        "ERROR",
                        "POSTPROCESSING_FAILED",
                        "扫描后处理未完成，可在条件恢复后重试",
                        "{}",
                    )
                    .await;
                }
                Ok(false) => {}
                Err(error) => {
                    self.flush_home_after_scan_terminal().await;
                    return Err(error.into());
                }
            }
            self.flush_home_after_scan_terminal().await;
        }
        result
    }

    async fn run_to_completion_with_metadata_and_thumbnails_inner(
        &self,
        job_id: &str,
        batch_size: usize,
        probe: Option<MediaProbeService>,
        metadata: Option<MetadataReidentifyService>,
        thumbnails: Option<ThumbnailService>,
    ) -> Result<(), ScanJobError> {
        let manifest_workflow_version = self
            .database
            .get_scan_manifest_by_job(job_id)
            .await?
            .filter(|manifest| manifest.discovery_format_version == 3)
            .map(|manifest| manifest.workflow_version);
        let uses_local_metadata_outbox = manifest_workflow_version == Some(3);
        let defer_local_metadata_worker = matches!(manifest_workflow_version, Some(2 | 3));
        if uses_local_metadata_outbox
            && let Err(error) = self.start_local_metadata_outbox_worker().await
        {
            tracing::warn!(job_id, %error, "local metadata outbox worker did not start");
        }
        let mut scan_permit = self.acquire_scan_lock_for_job(job_id).await?;
        let mut local_metadata_worker =
            (!defer_local_metadata_worker).then(|| self.start_local_metadata_worker(job_id));
        let mut created_items = 0_usize;
        loop {
            let report = match self
                .run_batch_with_failure_handling(job_id, batch_size, true)
                .await
            {
                Ok(report) => report,
                Err(error) => {
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    return Err(error);
                }
            };
            if report.processed > 0
                && !report.completed
                && let Some(home) = &self.home
            {
                home.invalidate_scan_batch().await;
            }
            created_items = created_items.saturating_add(report.created_items);
            if !report.completed {
                if self.should_yield_to_realtime(job_id).await? {
                    drop(scan_permit);
                    scan_permit = self.acquire_scan_lock_for_job(job_id).await?;
                }
                tokio::task::yield_now().await;
                continue;
            }
            if report.status == "COMPLETED" {
                let Some(completed_job) = self.database.find_scan_job(job_id).await? else {
                    if self.cancellation_requested_in_memory(job_id) {
                        Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                        self.clear_cancellation_flag(job_id);
                        return Ok(());
                    }
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    return Err(ScanJobError::JobNotFound);
                };
                let incremental = completed_job.job_type == "INCREMENTAL_SCAN";
                if !incremental && defer_local_metadata_worker {
                    if let Err(error) = self
                        .materialize_manifest_postprocessing_targets_unlocked(job_id)
                        .await
                    {
                        Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                        return Err(error);
                    }
                    if let Err(error) = self
                        .verify_manifest_postprocessing_target_roots(job_id)
                        .await
                    {
                        Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                        return Err(error);
                    }
                    if !uses_local_metadata_outbox {
                        local_metadata_worker = Some(self.start_local_metadata_worker(job_id));
                    }
                }
                drop(scan_permit);
                if self
                    .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                    .await?
                {
                    return Ok(());
                }
                if incremental {
                    if let Err(error) = self.wait_for_local_metadata(job_id).await {
                        Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                        return Err(error);
                    }
                    if self
                        .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                        .await?
                    {
                        return Ok(());
                    }
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    self.run_probe_after_scan(job_id, probe).await?;
                    if self
                        .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                        .await?
                    {
                        return Ok(());
                    }
                    self.run_thumbnails_after_incremental_scan(job_id, thumbnails)
                        .await?;
                    if self
                        .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                        .await?
                    {
                        return Ok(());
                    }
                    for target_type in ["SOURCE", "ITEM"] {
                        self.database
                            .skip_pending_scan_job_target_stage(job_id, target_type, "THUMBNAIL")
                            .await?;
                    }
                    self.database
                        .clear_completed_scan_job_targets(job_id)
                        .await?;
                    let strm_probe_scheduled = if completed_job.auto_metadata_match {
                        if !uses_local_metadata_outbox
                            && self.metadata_selection.is_none()
                            && let Some(metadata) = metadata
                        {
                            self.schedule_online_metadata_after_incremental_scan(job_id, metadata)
                                .await;
                        }
                        if let Some(strm_probe) = self.strm_probe.clone() {
                            self.schedule_strm_probe_after_incremental_scan(
                                job_id,
                                &completed_job.library_id,
                                strm_probe,
                            )
                            .await
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    self.run_auto_library_cover_after_scan(job_id).await?;
                    self.flush_home_after_scan_terminal().await;
                    self.publish_media_added_event(&completed_job, created_items)
                        .await;
                    if !strm_probe_scheduled {
                        self.database.clear_scan_job_paths(job_id).await?;
                    }
                    return Ok(());
                }
                if let Err(error) = self.database.retry_failed_scan_job_targets(job_id).await {
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    return Err(error.into());
                }
                if uses_local_metadata_outbox {
                    for target_type in ["SOURCE", "ITEM"] {
                        self.database
                            .skip_pending_scan_job_target_stage(job_id, target_type, "METADATA")
                            .await?;
                    }
                }
                if let Err(error) = self.wait_for_local_metadata(job_id).await {
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    return Err(error);
                }
                if self
                    .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                    .await?
                {
                    return Ok(());
                }
                Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                self.update_activity(job_id, Some("媒体探测"), "POSTPROCESSING")
                    .await?;
                if defer_local_metadata_worker {
                    self.verify_manifest_postprocessing_target_roots(job_id)
                        .await?;
                }
                self.run_probe_after_scan(job_id, probe).await?;
                if self
                    .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                    .await?
                {
                    return Ok(());
                }
                self.update_activity(job_id, Some("本地元数据"), "POSTPROCESSING")
                    .await?;
                if defer_local_metadata_worker {
                    self.verify_manifest_postprocessing_target_roots(job_id)
                        .await?;
                }
                self.run_metadata_after_scan(job_id).await?;
                if self
                    .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                    .await?
                {
                    return Ok(());
                }
                self.update_activity(job_id, Some("媒体库封面"), "POSTPROCESSING")
                    .await?;
                self.run_auto_library_cover_after_scan(job_id).await?;
                if self
                    .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                    .await?
                {
                    return Ok(());
                }
                self.update_activity(job_id, Some("视频缩略图"), "POSTPROCESSING")
                    .await?;
                if defer_local_metadata_worker {
                    self.verify_manifest_postprocessing_target_roots(job_id)
                        .await?;
                }
                if uses_local_metadata_outbox && thumbnails.is_some() {
                    self.wait_for_local_metadata_images(job_id).await?;
                    if self
                        .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                        .await?
                    {
                        return Ok(());
                    }
                }
                self.run_thumbnails_after_scan(job_id, thumbnails).await?;
                if self
                    .finish_cancellation_if_requested(job_id, &mut local_metadata_worker)
                    .await?
                {
                    return Ok(());
                }
                self.database
                    .clear_completed_scan_job_targets(job_id)
                    .await?;
                let completed = self
                    .database
                    .complete_scan_job_postprocessing(job_id)
                    .await?;
                if !completed {
                    if self.database.fail_scan_job_postprocessing(job_id).await? {
                        self.record_event(
                            job_id,
                            "ERROR",
                            "POSTPROCESSING_FAILED",
                            "扫描后处理失败，可重试未完成目标",
                            "{}",
                        )
                        .await;
                    }
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    self.flush_home_after_scan_terminal().await;
                    return Ok(());
                }
                if let Err(error) = self
                    .database
                    .cleanup_completed_scan_manifest_payloads()
                    .await
                {
                    tracing::warn!(job_id, %error, "completed scan manifest payload cleanup failed");
                }
                self.database.spawn_removed_media_item_purge();
                if completed_job.auto_metadata_match && !uses_local_metadata_outbox {
                    if let Some(metadata) = metadata {
                        self.schedule_online_metadata_after_scan(job_id, metadata)
                            .await;
                    }
                }
                self.flush_home_after_scan_terminal().await;
                self.publish_media_added_event(&completed_job, created_items)
                    .await;
            }
            Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
            return Ok(());
        }
    }

    async fn run_auto_library_cover_after_scan(&self, job_id: &str) -> Result<(), ScanJobError> {
        let Some(covers) = self.library_covers.as_ref() else {
            return Ok(());
        };
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Ok(());
        };
        let Ok(library_id) = job.library_id.parse::<LibraryId>() else {
            tracing::warn!(
                job_id,
                library_id = %job.library_id,
                "automatic library cover generation skipped for invalid library ID"
            );
            return Ok(());
        };
        match covers.generate_if_eligible(library_id).await {
            Ok(AutoLibraryCoverResult::Generated) => {
                self.record_event(
                    job_id,
                    "INFO",
                    "LIBRARY_COVER_GENERATED",
                    "已自动生成媒体库封面",
                    "{}",
                )
                .await;
            }
            Ok(
                AutoLibraryCoverResult::BelowThreshold
                | AutoLibraryCoverResult::ExistingCover
                | AutoLibraryCoverResult::TaskNotRegistered
                | AutoLibraryCoverResult::AlreadyHandled,
            ) => {}
            Err(error) => {
                tracing::warn!(job_id, %error, "automatic library cover generation failed");
                self.record_event(
                    job_id,
                    "ERROR",
                    "LIBRARY_COVER_FAILED",
                    "自动媒体库封面生成失败",
                    "{}",
                )
                .await;
            }
        }
        Ok(())
    }

    async fn acquire_scan_lock(&self) -> Result<OwnedSemaphorePermit, ScanJobError> {
        Arc::clone(&self.scan_lock)
            .acquire_owned()
            .await
            .map_err(|_| ScanJobError::ScanLockClosed)
    }

    async fn acquire_scan_lock_for_job(
        &self,
        job_id: &str,
    ) -> Result<OwnedSemaphorePermit, ScanJobError> {
        let mut database_wait_retry = 0_usize;
        loop {
            let job = self.database.find_scan_job(job_id).await?;
            let Some(job) = job else {
                return self.acquire_scan_lock().await;
            };
            if job.job_type == "INCREMENTAL_SCAN"
                && self
                    .database
                    .has_unready_manifest_target_materialization_for_library(&job.library_id)
                    .await?
            {
                tokio::time::sleep(scan_lock_database_wait_backoff(database_wait_retry)).await;
                database_wait_retry = database_wait_retry.saturating_add(1);
                continue;
            }
            let is_postprocessing_target_materializer = job.job_type == "RECONCILE_LIBRARY"
                && job.status == "COMPLETED"
                && job.scan_phase == "POSTPROCESSING"
                && self
                    .database
                    .has_unready_manifest_target_materialization_for_library(&job.library_id)
                    .await?;
            let is_full_scan = (job.job_type != "INCREMENTAL_SCAN"
                && matches!(job.status.as_str(), "PENDING" | "RUNNING")
                && job.scan_phase != "POSTPROCESSING"
                && !job.cancel_requested)
                || is_postprocessing_target_materializer;
            let has_incremental = if is_postprocessing_target_materializer {
                self.database
                    .has_running_scan_job_type("INCREMENTAL_SCAN")
                    .await?
            } else if is_full_scan {
                self.database
                    .has_active_scan_job_type("INCREMENTAL_SCAN")
                    .await?
            } else {
                false
            };
            if is_full_scan && has_incremental {
                tokio::time::sleep(scan_lock_database_wait_backoff(database_wait_retry)).await;
                database_wait_retry = database_wait_retry.saturating_add(1);
                continue;
            }

            let permit = self.acquire_scan_lock().await?;
            let has_incremental = if is_postprocessing_target_materializer {
                self.database
                    .has_running_scan_job_type("INCREMENTAL_SCAN")
                    .await?
            } else if is_full_scan {
                self.database
                    .has_active_scan_job_type("INCREMENTAL_SCAN")
                    .await?
            } else {
                false
            };
            if !is_full_scan || !has_incremental {
                return Ok(permit);
            }
            drop(permit);
            tokio::time::sleep(scan_lock_database_wait_backoff(database_wait_retry)).await;
            database_wait_retry = database_wait_retry.saturating_add(1);
        }
    }

    async fn should_yield_to_realtime(&self, job_id: &str) -> Result<bool, ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Ok(false);
        };
        if job.job_type == "INCREMENTAL_SCAN"
            || !matches!(job.status.as_str(), "PENDING" | "RUNNING")
            || job.scan_phase == "POSTPROCESSING"
            || job.cancel_requested
        {
            return Ok(false);
        }
        self.database
            .has_active_scan_job_type("INCREMENTAL_SCAN")
            .await
            .map_err(ScanJobError::from)
    }

    async fn effective_scan_concurrency(&self, configured: i64) -> usize {
        self.resources
            .background_concurrency(usize::try_from(configured).unwrap_or(1))
            .await
    }

    async fn run_thumbnails_after_scan(
        &self,
        job_id: &str,
        thumbnails: Option<ThumbnailService>,
    ) -> Result<(), ScanJobError> {
        if self.database.find_scan_job(job_id).await?.is_none() {
            return Ok(());
        }
        let Some(thumbnails) = thumbnails else {
            self.database
                .skip_pending_scan_job_target_stage(job_id, "ITEM", "THUMBNAIL")
                .await?;
            return Ok(());
        };
        self.database
            .ensure_scan_job_thumbnail_targets(job_id)
            .await?;
        let started = Instant::now();
        match thumbnails.generate_scan_job(job_id).await {
            Ok(report) if report.failed == 0 => {
                let items = report
                    .considered
                    .saturating_add(report.skipped_strm)
                    .saturating_add(report.skipped_policy);
                let elapsed_ms = started.elapsed().as_millis();
                let details = format!(
                    r#"{{"considered":{},"generated":{},"reused":{},"failed":{},"skippedStrm":{},"skippedPolicy":{},"elapsedMs":{},"itemsPerSecond":{}}}"#,
                    report.considered,
                    report.generated,
                    report.reused,
                    report.failed,
                    report.skipped_strm,
                    report.skipped_policy,
                    elapsed_ms,
                    throughput_per_second(items, elapsed_ms),
                );
                self.record_event(
                    job_id,
                    "INFO",
                    "THUMBNAIL_COMPLETED",
                    "视频缩略图任务完成",
                    &details,
                )
                .await;
            }
            Ok(report) => {
                let items = report
                    .considered
                    .saturating_add(report.skipped_strm)
                    .saturating_add(report.skipped_policy);
                let elapsed_ms = started.elapsed().as_millis();
                let details = format!(
                    r#"{{"considered":{},"generated":{},"reused":{},"failed":{},"skippedStrm":{},"skippedPolicy":{},"elapsedMs":{},"itemsPerSecond":{}}}"#,
                    report.considered,
                    report.generated,
                    report.reused,
                    report.failed,
                    report.skipped_strm,
                    report.skipped_policy,
                    elapsed_ms,
                    throughput_per_second(items, elapsed_ms),
                );
                self.record_event(
                    job_id,
                    "WARN",
                    "THUMBNAIL_FAILED",
                    "部分视频缩略图生成失败",
                    &details,
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(job_id, %error, "scan thumbnail task failed");
                self.record_event(
                    job_id,
                    "ERROR",
                    "THUMBNAIL_FAILED",
                    "视频缩略图任务失败",
                    &format!(
                        r#"{{"elapsedMs":{},"itemsPerSecond":0}}"#,
                        started.elapsed().as_millis()
                    ),
                )
                .await;
            }
        }
        Ok(())
    }

    async fn schedule_online_metadata_after_scan(
        &self,
        scan_job_id: &str,
        metadata: MetadataReidentifyService,
    ) {
        let Some(scan_job) = self
            .database
            .find_scan_job(scan_job_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let Some(library) = self
            .database
            .find_library(&scan_job.library_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        if library.scraper_id.as_deref().is_none() {
            return;
        }
        let Ok(library_id) = scan_job.library_id.parse::<LibraryId>() else {
            tracing::warn!(
                scan_job_id,
                library_id = %scan_job.library_id,
                "automatic metadata matching skipped for invalid library ID"
            );
            return;
        };
        let job = match metadata
            .create_library_refresh_job(&library_id.to_string(), MetadataRefreshMode::FillMissing)
            .await
        {
            Ok(job) => job,
            Err(MetadataReidentifyError::InvalidItemCount) => return,
            Err(_) => {
                tracing::warn!(
                    scan_job_id,
                    "scan completed but automatic metadata matching could not be queued"
                );
                self.record_event(
                    scan_job_id,
                    "ERROR",
                    "METADATA_AUTO_MATCH_QUEUE_FAILED",
                    "自动元数据匹配任务创建失败",
                    "{}",
                )
                .await;
                return;
            }
        };
        if let Err(error) = metadata.enqueue_fill_missing_job(&job).await {
            tracing::warn!(
                scan_job_id,
                %error,
                "scan completed but automatic metadata matching could not be dispatched"
            );
            self.record_event(
                scan_job_id,
                "ERROR",
                "METADATA_AUTO_MATCH_QUEUE_FAILED",
                "自动元数据匹配任务创建失败",
                "{}",
            )
            .await;
            return;
        }
        let details = format!(
            r#"{{"itemCount":{},"jobId":"{}","mode":"FILL_MISSING"}}"#,
            job.total_count, job.id
        );
        self.record_event(
            scan_job_id,
            "INFO",
            "METADATA_AUTO_MATCH_QUEUED",
            "已提交自动元数据匹配任务",
            &details,
        )
        .await;
    }

    async fn schedule_online_metadata_after_incremental_scan(
        &self,
        scan_job_id: &str,
        metadata: MetadataReidentifyService,
    ) {
        let Some(scan_job) = self
            .database
            .find_scan_job(scan_job_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let Some(library) = self
            .database
            .find_library(&scan_job.library_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        if library
            .scraper_id
            .as_deref()
            .is_none_or(|scraper_id| scraper_id.trim().is_empty())
        {
            return;
        }
        let Ok(item_ids) = self
            .database
            .list_media_item_ids_for_incremental_scan(scan_job_id)
            .await
        else {
            tracing::warn!(
                scan_job_id,
                "incremental scan completed but affected media items could not be found"
            );
            return;
        };
        for item_ids in item_ids.chunks(100) {
            let job = match metadata.create_fill_missing_job(item_ids.to_vec()).await {
                Ok(job) => job,
                Err(_) => {
                    tracing::warn!(
                        scan_job_id,
                        item_count = item_ids.len(),
                        "incremental scan completed but automatic metadata matching could not be queued"
                    );
                    self.record_event(
                        scan_job_id,
                        "ERROR",
                        "METADATA_AUTO_MATCH_QUEUE_FAILED",
                        "自动元数据匹配任务创建失败",
                        "{}",
                    )
                    .await;
                    continue;
                }
            };
            if let Err(error) = metadata.enqueue_fill_missing_job(&job).await {
                tracing::warn!(
                    scan_job_id,
                    item_count = item_ids.len(),
                    %error,
                    "incremental scan completed but automatic metadata matching could not be dispatched"
                );
                self.record_event(
                    scan_job_id,
                    "ERROR",
                    "METADATA_AUTO_MATCH_QUEUE_FAILED",
                    "自动元数据匹配任务创建失败",
                    "{}",
                )
                .await;
                continue;
            }
            let details = format!(
                r#"{{"itemCount":{},"jobId":"{}","mode":"FILL_MISSING"}}"#,
                job.total_count, job.id
            );
            self.record_event(
                scan_job_id,
                "INFO",
                "METADATA_AUTO_MATCH_QUEUED",
                "已提交自动元数据匹配任务",
                &details,
            )
            .await;
        }
    }

    async fn schedule_strm_probe_after_incremental_scan(
        &self,
        scan_job_id: &str,
        library_id: &str,
        strm_probe: StrmProbeService,
    ) -> bool {
        let Ok(library_id) = library_id.parse::<LibraryId>() else {
            tracing::warn!(
                scan_job_id,
                library_id,
                "incremental scan completed but automatic STRM probe skipped for invalid library ID"
            );
            return false;
        };
        let job = match strm_probe
            .create_configured_incremental_job(scan_job_id, library_id)
            .await
        {
            Ok(Some(job)) => job,
            Ok(None) => return false,
            Err(error) => {
                tracing::warn!(
                    scan_job_id,
                    %error,
                    "incremental scan completed but automatic STRM probe could not be queued"
                );
                return false;
            }
        };
        let job_id = job.id.clone();
        let worker = strm_probe;
        let database = self.database.clone();
        let scan_job_id_for_cleanup = scan_job_id.to_owned();
        tokio::spawn(async move {
            if let Err(error) = worker.run(&job_id).await {
                tracing::error!(job_id = %job_id, %error, "automatic STRM probe stopped");
            }
            if let Err(error) = database
                .clear_scan_job_paths(&scan_job_id_for_cleanup)
                .await
            {
                tracing::warn!(
                    scan_job_id = %scan_job_id_for_cleanup,
                    %error,
                    "automatic STRM probe finished but scan paths could not be cleared"
                );
            }
        });
        self.record_event(
            scan_job_id,
            "INFO",
            "STRM_MEDIA_INFO_AUTO_QUEUED",
            "已提交新增 STRM 媒体信息识别任务",
            &format!(
                r#"{{"jobId":"{}","itemCount":{}}}"#,
                job.id, job.total_count
            ),
        )
        .await;
        true
    }

    async fn run_metadata_after_scan(&self, job_id: &str) -> Result<(), ScanJobError> {
        if self
            .database
            .get_scan_manifest_by_job(job_id)
            .await?
            .is_some_and(|manifest| manifest.workflow_version == 3)
        {
            return Ok(());
        }
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Ok(());
        };
        if self.database.find_library(&job.library_id).await?.is_none() {
            return Ok(());
        }
        let enricher = MetadataEnricher::new(self.database.clone())
            .with_resource_metrics(self.resources.clone());
        let enricher = match self.people.clone() {
            Some(people) => enricher.with_people(people),
            None => enricher,
        };
        let enricher = match self.local_nfo.clone() {
            Some(local_nfo) => enricher.with_nfo_store(local_nfo),
            None => enricher,
        };
        let started = Instant::now();
        let result = enricher.enrich_scan_job(job_id).await;
        match result {
            Ok(report) => {
                let items = report
                    .nfo_loaded
                    .saturating_add(report.nfo_failed)
                    .saturating_add(report.nfo_skipped);
                let elapsed_ms = started.elapsed().as_millis();
                let details = format!(
                    r#"{{"nfoLoaded":{},"nfoFailed":{},"nfoSkipped":{},"imagesFound":{},"elapsedMs":{},"itemsPerSecond":{}}}"#,
                    report.nfo_loaded,
                    report.nfo_failed,
                    report.nfo_skipped,
                    report.images_found,
                    elapsed_ms,
                    throughput_per_second(items, elapsed_ms),
                );
                self.record_event(
                    job_id,
                    "INFO",
                    "METADATA_COMPLETED",
                    "本地元数据处理完成",
                    &details,
                )
                .await;
            }
            Err(_) => {
                tracing::warn!(
                    job_id,
                    "scan completed but local metadata enrichment failed"
                );
                self.record_event(
                    job_id,
                    "ERROR",
                    "METADATA_FAILED",
                    "本地元数据处理失败",
                    &format!(
                        r#"{{"elapsedMs":{},"itemsPerSecond":0}}"#,
                        started.elapsed().as_millis()
                    ),
                )
                .await;
            }
        }
        Ok(())
    }

    async fn run_thumbnails_after_incremental_scan(
        &self,
        job_id: &str,
        thumbnails: Option<ThumbnailService>,
    ) -> Result<(), ScanJobError> {
        if self.database.find_scan_job(job_id).await?.is_none() {
            return Ok(());
        }
        let Some(thumbnails) = thumbnails else {
            return Ok(());
        };
        match thumbnails.generate_incremental_scan(job_id).await {
            Ok(report) if report.failed == 0 => {
                let details = format!(
                    r#"{{"considered":{},"generated":{},"reused":{},"failed":{},"skippedStrm":{},"skippedPolicy":{}}}"#,
                    report.considered,
                    report.generated,
                    report.reused,
                    report.failed,
                    report.skipped_strm,
                    report.skipped_policy,
                );
                self.record_event(
                    job_id,
                    "INFO",
                    "THUMBNAIL_COMPLETED",
                    "局部扫描视频缩略图任务完成",
                    &details,
                )
                .await;
            }
            Ok(report) => {
                let details = format!(
                    r#"{{"considered":{},"generated":{},"reused":{},"failed":{},"skippedStrm":{},"skippedPolicy":{}}}"#,
                    report.considered,
                    report.generated,
                    report.reused,
                    report.failed,
                    report.skipped_strm,
                    report.skipped_policy,
                );
                self.record_event(
                    job_id,
                    "WARN",
                    "THUMBNAIL_FAILED",
                    "局部扫描视频缩略图任务部分失败",
                    &details,
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(job_id, %error, "incremental scan thumbnail task failed");
                self.record_event(
                    job_id,
                    "WARN",
                    "THUMBNAIL_FAILED",
                    "局部扫描视频缩略图任务失败",
                    "{}",
                )
                .await;
            }
        }
        Ok(())
    }

    async fn run_probe_after_scan(
        &self,
        job_id: &str,
        probe: Option<MediaProbeService>,
    ) -> Result<(), ScanJobError> {
        if self.database.find_scan_job(job_id).await?.is_none() {
            return Ok(());
        }
        let Some(probe) = probe else {
            self.database
                .skip_pending_scan_job_target_stage(job_id, "SOURCE", "PROBE")
                .await?;
            return Ok(());
        };
        let job = self
            .database
            .find_scan_job(job_id)
            .await?
            .ok_or(ScanJobError::JobNotFound)?;
        let started = Instant::now();
        match probe.probe_scan_job(job_id, &job.library_id).await {
            Ok(report) => {
                let items = report.attempted.saturating_add(report.skipped);
                let elapsed_ms = started.elapsed().as_millis();
                let details = format!(
                    r#"{{"attempted":{},"ready":{},"failed":{},"timedOut":{},"skipped":{},"elapsedMs":{},"itemsPerSecond":{}}}"#,
                    report.attempted,
                    report.ready,
                    report.failed,
                    report.timed_out,
                    report.skipped,
                    elapsed_ms,
                    throughput_per_second(items, elapsed_ms),
                );
                self.record_event(job_id, "INFO", "PROBE_COMPLETED", "媒体探测完成", &details)
                    .await;
                self.database
                    .skip_pending_scan_job_target_stage(job_id, "SOURCE", "PROBE")
                    .await?;
            }
            Err(error) => {
                tracing::warn!(job_id, %error, "scan completed but media probe failed");
                self.record_event(
                    job_id,
                    "ERROR",
                    "PROBE_FAILED",
                    "媒体探测任务失败",
                    &format!(
                        r#"{{"elapsedMs":{},"itemsPerSecond":0}}"#,
                        started.elapsed().as_millis()
                    ),
                )
                .await;
            }
        }
        Ok(())
    }

    pub async fn active_job_ids(&self) -> Result<Vec<String>, ScanJobError> {
        Ok(self.database.list_scan_job_ids_needing_resume().await?)
    }

    pub async fn cancel(&self, job_id: &str) -> Result<(), ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Err(ScanJobError::JobNotFound);
        };
        if !matches!(job.status.as_str(), "PENDING" | "RUNNING") {
            return Ok(());
        }
        let cancellation = self.cancellation_flag(job_id);
        cancellation.store(true, Ordering::Release);
        if job.status == "PENDING" {
            self.database.request_scan_job_cancel(job_id).await?;
            self.cancel_running_job(job_id).await?;
            return Ok(());
        }
        self.database.request_scan_job_cancel(job_id).await?;
        self.notify_local_metadata_worker(job_id);
        self.record_event(job_id, "INFO", "CANCEL_REQUESTED", "已请求取消任务", "{}")
            .await;
        Ok(())
    }

    pub async fn retry(&self, job_id: &str) -> Result<ScanJob, ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Err(ScanJobError::JobNotFound);
        };
        let can_retry_completed_postprocessing = if job.status == "COMPLETED"
            && job.job_type == "RECONCILE_LIBRARY"
            && job.scan_phase == "IDLE"
        {
            let legacy_work_is_cleared = !self
                .database
                .has_reconciliation_scan_entries(&job.id)
                .await?;
            let has_targets = self.database.has_scan_job_targets(&job.id).await?;
            let targets_need_materialization = self
                .database
                .has_unready_scan_manifest_postprocessing_targets(&job.id)
                .await?;
            legacy_work_is_cleared && (has_targets || targets_need_materialization)
        } else {
            false
        };
        if !matches!(job.status.as_str(), "FAILED" | "CANCELLED")
            && !can_retry_completed_postprocessing
        {
            return Err(ScanJobError::AlreadyActive(job.id));
        }
        let Ok(library_id) = job.library_id.parse::<LibraryId>() else {
            return Err(ScanJobError::LibraryNotFound);
        };
        if let Some(active) = self
            .database
            .find_active_scan_job(&job.library_id, &job.job_type)
            .await?
        {
            return Err(ScanJobError::AlreadyActive(active.id));
        }
        if job.job_type == "RECONCILE_LIBRARY" {
            let manifest_stage = self.database.prepare_scan_manifest_retry(&job.id).await?;
            match manifest_stage.as_deref() {
                Some("POSTPROCESSING" | "INDEXED") => {
                    if self.database.retry_scan_job_postprocessing(&job.id).await? {
                        return self.get_job(&job.id).await;
                    }
                    return self
                        .create_movie_scan_job_with_metadata(library_id, job.auto_metadata_match)
                        .await;
                }
                Some("DISCOVERING" | "READY_TO_DIFF" | "APPLYING") => {
                    if !self.database.retry_scan_job(&job.id).await? {
                        return Err(ScanJobError::AlreadyActive(job.id));
                    }
                    return self.get_job(&job.id).await;
                }
                Some("RESET_REQUIRED") | None => {
                    return self
                        .create_movie_scan_job_with_metadata_and_legacy_retry(
                            library_id,
                            job.auto_metadata_match,
                            Some(&job.id),
                        )
                        .await;
                }
                Some("COMPLETED") => {
                    return self
                        .create_movie_scan_job_with_metadata(library_id, job.auto_metadata_match)
                        .await;
                }
                Some(stage) => {
                    return Err(ScanJobError::Storage(StorageError::Conflict(format!(
                        "unsupported manifest retry stage {stage}"
                    ))));
                }
            }
        }
        if !self.database.retry_scan_job(&job.id).await? {
            return Err(ScanJobError::AlreadyActive(job.id));
        }
        self.get_job(&job.id).await
    }

    async fn get_job(&self, id: &str) -> Result<ScanJob, ScanJobError> {
        self.database
            .find_scan_job(id)
            .await?
            .map(scan_job)
            .ok_or(ScanJobError::JobNotFound)
    }

    async fn record_event(
        &self,
        job_id: &str,
        level: &str,
        event_code: &str,
        message: &str,
        details_json: &str,
    ) {
        let id = Uuid::now_v7().to_string();
        if let Err(error) = self
            .log_store
            .append_scan_job_event(&id, job_id, level, event_code, message, details_json)
            .await
        {
            tracing::error!(
                job_id,
                event_code,
                error_kind = ?error.kind(),
                "failed to persist scan job log event"
            );
        }
        if event_code == "DISCOVERY_PROGRESS" {
            self.admin_events.publish_jobs_progress();
        } else {
            self.admin_events.publish_jobs_now();
        }
    }
}

type MoviePreparationOutput = (
    usize,
    String,
    StoredReconciliationScanEntry,
    Option<NewMovieFile>,
);
type MoviePreparationTask = Result<MoviePreparationOutput, ScannerError>;
type MovieFingerprintTask = Result<(usize, MovieQuickScanResult), ScannerError>;
type EpisodeQuickResult = Option<(String, ScanReport, Option<(String, String)>)>;
type EpisodeFingerprintTask = Result<(usize, EpisodeQuickResult), ScannerError>;
type ReconciliationFingerprintTask = Result<(usize, Option<(String, ScanReport)>), ScannerError>;
type ReconciliationRegularTask = Result<(usize, ScanReport), ScannerError>;
type ReconciliationRegularGroupTask = Result<Vec<(usize, ScanReport)>, ScannerError>;

struct ReconciliationScanWork {
    entry: StoredReconciliationScanEntry,
    path: PathBuf,
    classification: MixedClassification,
}

#[derive(Clone)]
struct ReconciliationRegularWork {
    index: usize,
    entry: StoredReconciliationScanEntry,
    root: StoredLibraryRoot,
    path: PathBuf,
    classification: MixedClassification,
}

enum MovieQuickScanResult {
    Unchanged {
        entry_id: String,
        report: ScanReport,
    },
    Rescan,
    RescanWithVariantSuffix(String),
}

fn reconciliation_regular_group_key(
    root_id: &str,
    path: &Path,
    classification: MixedClassification,
    inferred_suffix: Option<&str>,
    fallback_index: usize,
) -> String {
    let parsed_key = match classification {
        MixedClassification::Movie => path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| {
                inferred_suffix
                    .and_then(|suffix| parse_movie_filename_with_variant_suffix(name, suffix))
                    .or_else(|| parse_movie_filename(name))
            })
            .map(|parsed| {
                format!(
                    "movie:{}:{:?}:{:?}",
                    parsed.sort_title, parsed.production_year, parsed.provider_ids
                )
            }),
        MixedClassification::Episode => path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(parse_episode_filename)
            .map(|parsed| {
                format!(
                    "episode:{}:{:?}:{:?}:{:?}",
                    parsed.sort_title, parsed.production_year, parsed.season, parsed.episode
                )
            }),
        MixedClassification::Unresolved => None,
    };
    parsed_key.map_or_else(
        || format!("path:{root_id}:{fallback_index}"),
        |key| format!("{root_id}:{key}"),
    )
}

async fn collect_movie_fingerprint_task(
    tasks: &mut JoinSet<MovieFingerprintTask>,
    results: &mut [MovieQuickScanResult],
) -> Result<(), ScannerError> {
    let (index, result) = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<movie-fingerprint-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<movie-fingerprint-task>"),
                source: std::io::Error::other("movie fingerprint task set is empty"),
            });
        }
    };
    if let Some(slot) = results.get_mut(index) {
        *slot = result;
    }
    Ok(())
}

async fn collect_episode_fingerprint_task(
    tasks: &mut JoinSet<EpisodeFingerprintTask>,
    results: &mut [EpisodeQuickResult],
) -> Result<(), ScannerError> {
    let (index, result) = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<episode-fingerprint-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<episode-fingerprint-task>"),
                source: std::io::Error::other("episode fingerprint task set is empty"),
            });
        }
    };
    if let Some(slot) = results.get_mut(index) {
        *slot = result;
    }
    Ok(())
}

async fn collect_reconciliation_fingerprint_task(
    tasks: &mut JoinSet<ReconciliationFingerprintTask>,
    results: &mut [Option<(String, ScanReport)>],
) -> Result<(), ScannerError> {
    let (index, result) = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<reconciliation-fingerprint-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<reconciliation-fingerprint-task>"),
                source: std::io::Error::other("reconciliation fingerprint task set is empty"),
            });
        }
    };
    if let Some(slot) = results.get_mut(index) {
        *slot = result;
    }
    Ok(())
}

async fn collect_reconciliation_regular_task(
    tasks: &mut JoinSet<ReconciliationRegularTask>,
    results: &mut [Option<ScanReport>],
) -> Result<(), ScannerError> {
    let (index, result) = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<reconciliation-regular-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<reconciliation-regular-task>"),
                source: std::io::Error::other("reconciliation regular task set is empty"),
            });
        }
    };
    if let Some(slot) = results.get_mut(index) {
        *slot = Some(result);
    }
    Ok(())
}

async fn collect_reconciliation_regular_group_task(
    tasks: &mut JoinSet<ReconciliationRegularGroupTask>,
    results: &mut [Option<ScanReport>],
) -> Result<(), ScannerError> {
    let reports = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<reconciliation-regular-group-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<reconciliation-regular-group-task>"),
                source: std::io::Error::other("reconciliation regular group task set is empty"),
            });
        }
    };
    for (index, report) in reports {
        if let Some(slot) = results.get_mut(index) {
            *slot = Some(report);
        }
    }
    Ok(())
}

async fn join_movie_preparation(
    tasks: &mut JoinSet<MoviePreparationTask>,
) -> Result<MoviePreparationOutput, ScannerError> {
    match tasks.join_next().await {
        Some(Ok(result)) => result,
        Some(Err(error)) => Err(ScannerError::Io {
            path: PathBuf::from("<scan-preparation-task>"),
            source: std::io::Error::other(error.to_string()),
        }),
        None => Err(ScannerError::Io {
            path: PathBuf::from("<scan-preparation-task>"),
            source: std::io::Error::other("scan preparation task set is empty"),
        }),
    }
}

async fn collect_movie_preparation_task(
    tasks: &mut JoinSet<Result<(usize, Option<NewMovieFile>), ScannerError>>,
    results: &mut Vec<(usize, Option<NewMovieFile>)>,
) -> Result<(), ScannerError> {
    let result = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<movie-preparation-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<movie-preparation-task>"),
                source: std::io::Error::other("movie preparation task set is empty"),
            });
        }
    };
    results.push(result);
    Ok(())
}

async fn collect_episode_preparation_task(
    tasks: &mut JoinSet<Result<(usize, Option<NewEpisodeFile>), ScannerError>>,
    results: &mut [Option<NewEpisodeFile>],
) -> Result<(), ScannerError> {
    let (index, result) = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<episode-preparation-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<episode-preparation-task>"),
                source: std::io::Error::other("episode preparation task set is empty"),
            });
        }
    };
    if let Some(slot) = results.get_mut(index) {
        *slot = result;
    }
    Ok(())
}

fn scan_job(job: StoredScanJob) -> ScanJob {
    ScanJob {
        id: job.id,
        library_id: job.library_id,
        job_type: job.job_type,
        status: job.status,
        generation: job.generation,
        cursor: job.cursor,
        processed_count: job.processed_count,
        total_count: job.total_count,
        discovery_completed: job.discovery_completed,
        cancel_requested: job.cancel_requested,
        error: job.error,
        created_at: job.created_at,
        started_at: job.started_at,
        finished_at: job.finished_at,
        current_item: job.current_item,
        scan_phase: job.scan_phase,
    }
}

fn sqlite_lock_contention_code(error: &StorageError) -> Option<&'static str> {
    let StorageError::Sqlx { source, .. } = error else {
        return None;
    };
    let code = source
        .as_database_error()
        .and_then(|database_error| database_error.code())
        .and_then(|code| code.parse::<i32>().ok())?;
    match code & 0xff {
        5 => Some("SQLITE_BUSY"),
        6 => Some("SQLITE_LOCKED"),
        _ => None,
    }
}

fn scanner_error_sqlite_lock_code(error: &ScannerError) -> Option<&'static str> {
    match error {
        ScannerError::Storage(error) => sqlite_lock_contention_code(error),
        _ => None,
    }
}

impl ScanJobError {
    fn code(&self) -> &'static str {
        match self {
            Self::LibraryNotFound => "LIBRARY_NOT_FOUND",
            Self::ItemNotFound => "ITEM_NOT_FOUND",
            Self::JobNotFound => "JOB_NOT_FOUND",
            Self::NoChanges => "NO_CHANGES",
            Self::AlreadyActive(_) => "ALREADY_ACTIVE",
            Self::InvalidBatchSize => "INVALID_BATCH_SIZE",
            Self::ScanLockClosed => "SCAN_LOCK_CLOSED",
            Self::LibraryDeletionScanTimeout => "LIBRARY_DELETION_SCAN_TIMEOUT",
            Self::Scanner(error) => error.code(),
            Self::Storage(error) => sqlite_lock_contention_code(error).unwrap_or("STORAGE_ERROR"),
        }
    }

    fn is_retryable_sqlite_lock_contention(&self) -> bool {
        matches!(self.code(), "SQLITE_BUSY" | "SQLITE_LOCKED")
    }
}

impl std::fmt::Display for ScanJobError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::ItemNotFound => formatter.write_str("media item not found"),
            Self::JobNotFound => formatter.write_str("scan job not found"),
            Self::NoChanges => formatter.write_str("incremental scan has no valid changes"),
            Self::AlreadyActive(id) => write!(formatter, "scan job already active: {id}"),
            Self::InvalidBatchSize => formatter.write_str("scan batch size must be positive"),
            Self::ScanLockClosed => formatter.write_str("scan lock is closed"),
            Self::LibraryDeletionScanTimeout => {
                formatter.write_str("timed out waiting for scans to stop before library deletion")
            }
            Self::Scanner(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

fn normalize_incremental_path(value: &str) -> Result<String, ScanJobError> {
    let value = value.trim().replace('\\', "/");
    let path = Path::new(&value);
    let has_windows_drive_prefix = value.as_bytes().get(1) == Some(&b':')
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic);
    if value.is_empty()
        || path.is_absolute()
        || has_windows_drive_prefix
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir
                    | Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(ScanJobError::Scanner(ScannerError::InvalidRelativePath(
            value,
        )));
    }
    Ok(value)
}

fn media_source_folder(value: &str) -> Result<String, ScanJobError> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ScanJobError::Scanner(ScannerError::InvalidRelativePath(
            value.to_owned(),
        )));
    }
    let folder = path
        .parent()
        .and_then(|parent| parent.to_str())
        .unwrap_or("");
    if folder.is_empty() {
        Ok(".".to_owned())
    } else {
        Ok(folder.to_owned())
    }
}

fn change_kind_name(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Create => "CREATE",
        ChangeKind::Modify => "MODIFY",
        ChangeKind::Rename => "RENAME",
        ChangeKind::Remove => "REMOVE",
    }
}

impl std::error::Error for ScanJobError {}

impl From<ScannerError> for ScanJobError {
    fn from(error: ScannerError) -> Self {
        Self::Scanner(error)
    }
}

impl From<StorageError> for ScanJobError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanReport {
    pub discovered_files: usize,
    pub created_items: usize,
    pub created_sources: usize,
    pub changed_files: usize,
    pub marked_missing: usize,
    pub unavailable_roots: usize,
    pub skipped_files: usize,
}

impl ScanReport {
    fn merge(&mut self, other: Self) {
        self.discovered_files += other.discovered_files;
        self.created_items += other.created_items;
        self.created_sources += other.created_sources;
        self.changed_files += other.changed_files;
        self.marked_missing += other.marked_missing;
        self.unavailable_roots += other.unavailable_roots;
        self.skipped_files += other.skipped_files;
    }
}

pub fn compute_file_fingerprint(
    relative_path: &str,
    size: i64,
    modified_at: i64,
    device: Option<u64>,
    inode: Option<u64>,
) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"LUX-FP-1\0");
    hasher.update((relative_path.len() as u64).to_le_bytes());
    hasher.update(relative_path.as_bytes());
    hasher.update(size.to_le_bytes());
    hasher.update(modified_at.to_le_bytes());
    hasher.update(device.unwrap_or_default().to_le_bytes());
    hasher.update(inode.unwrap_or_default().to_le_bytes());
    hasher.finalize().to_vec()
}

fn manifest_entry_observation(
    relative_path: String,
    entry_kind: &str,
    metadata: &std::fs::Metadata,
    path: &Path,
) -> Result<NewScanManifestEntry, ScannerError> {
    let size = i64::try_from(metadata.len())
        .map_err(|_| ScannerError::FileSizeOverflow(path.to_owned()))?;
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0);
    let (device, inode) = file_identity(metadata);
    let fingerprint = compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
    Ok(NewScanManifestEntry {
        relative_path,
        entry_kind: entry_kind.to_owned(),
        size,
        modified_at,
        device: device.and_then(|device| i64::try_from(device).ok()),
        inode: inode.and_then(|inode| i64::try_from(inode).ok()),
        fingerprint,
    })
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
fn manifest_entry_observation_from_stat(
    relative_path: String,
    entry_kind: &str,
    metadata: &libc::stat,
    path: &Path,
) -> Result<NewScanManifestEntry, ScannerError> {
    let size = u64::try_from(metadata.st_size)
        .ok()
        .and_then(|size| i64::try_from(size).ok())
        .ok_or_else(|| ScannerError::FileSizeOverflow(path.to_owned()))?;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let (modified_seconds, modified_nanoseconds) = (metadata.st_mtime, metadata.st_mtime_nsec);
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let (modified_seconds, modified_nanoseconds) = (metadata.st_mtime, metadata.st_mtime_nsec);
    let modified_at = i128::from(modified_seconds)
        .saturating_mul(1_000_000_000)
        .saturating_add(i128::from(modified_nanoseconds));
    let modified_at = if modified_at < 0 {
        0
    } else {
        i64::try_from(modified_at).unwrap_or(i64::MAX)
    };
    let device = u64::try_from(metadata.st_dev).ok();
    let inode: u64 = metadata.st_ino;
    let fingerprint =
        compute_file_fingerprint(&relative_path, size, modified_at, device, Some(inode));
    Ok(NewScanManifestEntry {
        relative_path,
        entry_kind: entry_kind.to_owned(),
        size,
        modified_at,
        device: device.and_then(|device| i64::try_from(device).ok()),
        inode: i64::try_from(inode).ok(),
        fingerprint,
    })
}

async fn current_file_fingerprint(
    root_path: &Path,
    path: &Path,
) -> Result<(String, Vec<u8>), ScannerError> {
    let relative_path = path
        .strip_prefix(root_path)
        .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
        .to_str()
        .ok_or(ScannerError::NonUtf8Path)?
        .to_owned();
    let metadata = fs::metadata(path)
        .await
        .map_err(|source| ScannerError::Io {
            path: path.to_owned(),
            source,
        })?;
    let size = i64::try_from(metadata.len())
        .map_err(|_| ScannerError::FileSizeOverflow(path.to_owned()))?;
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0);
    let (device, inode) = file_identity(&metadata);
    let fingerprint = compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
    Ok((relative_path, fingerprint))
}

fn file_identity(metadata: &std::fs::Metadata) -> (Option<u64>, Option<u64>) {
    #[cfg(unix)]
    {
        (Some(metadata.dev()), Some(metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        (None, None)
    }
}

fn safe_scan_activity_label(relative_path: &str) -> Option<String> {
    let trimmed = relative_path.trim_matches(|character| character == '/' || character == '\\');
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .rsplit(['/', '\\'])
        .find(|part| !part.trim().is_empty())
        .map(|part| {
            let mut label = part.trim().chars().take(160).collect::<String>();
            if label.contains('?') {
                label.truncate(label.find('?').unwrap_or(label.len()));
            }
            label
        })
        .filter(|label| !label.is_empty())
}

#[derive(Debug)]
pub enum ScannerError {
    LibraryNotFound,
    InvalidRootId(String),
    RootIdentityChanged(PathBuf),
    InvalidItemId(String),
    InvalidRelativePath(String),
    NonUtf8Path,
    FileSizeOverflow(PathBuf),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Storage(StorageError),
}

impl ScannerError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::LibraryNotFound => "LIBRARY_NOT_FOUND",
            Self::InvalidRootId(_) => "INVALID_ROOT_ID",
            Self::RootIdentityChanged(_) => "ROOT_IDENTITY_CHANGED",
            Self::InvalidItemId(_) => "INVALID_ITEM_ID",
            Self::InvalidRelativePath(_) => "INVALID_RELATIVE_PATH",
            Self::NonUtf8Path => "NON_UTF8_PATH",
            Self::FileSizeOverflow(_) => "FILE_SIZE_OVERFLOW",
            Self::Io { .. } => "SCAN_IO",
            Self::Storage(error) => sqlite_lock_contention_code(error).unwrap_or("STORAGE_ERROR"),
        }
    }
}

impl fmt::Display for ScannerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::InvalidRootId(error) => write!(formatter, "invalid library root ID: {error}"),
            Self::RootIdentityChanged(path) => {
                write!(
                    formatter,
                    "library root changed during scan: {}",
                    path.display()
                )
            }
            Self::InvalidItemId(error) => write!(formatter, "invalid media item ID: {error}"),
            Self::InvalidRelativePath(error) => write!(formatter, "invalid relative path: {error}"),
            Self::NonUtf8Path => formatter.write_str("path is not valid UTF-8"),
            Self::FileSizeOverflow(path) => {
                write!(formatter, "file size overflows i64: {}", path.display())
            }
            Self::Io { path, source } => {
                write!(formatter, "scan path '{}': {source}", path.display())
            }
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ScannerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Storage(error) => Some(error),
            Self::LibraryNotFound
            | Self::InvalidRootId(_)
            | Self::RootIdentityChanged(_)
            | Self::InvalidItemId(_)
            | Self::InvalidRelativePath(_)
            | Self::NonUtf8Path
            | Self::FileSizeOverflow(_) => None,
        }
    }
}

impl From<StorageError> for ScannerError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

fn throughput_per_second(items: usize, elapsed_ms: u128) -> u64 {
    if items == 0 {
        return 0;
    }
    let numerator = (items as u128).saturating_mul(1_000);
    let rate = numerator.checked_div(elapsed_ms).unwrap_or(numerator);
    u64::try_from(rate).unwrap_or(u64::MAX)
}

fn configured_scan_concurrency(
    global_override: Option<usize>,
    library_concurrency: Option<i64>,
    default_concurrency: usize,
) -> i64 {
    let configured = global_override
        .map(|value| i64::try_from(value).unwrap_or(i64::MAX))
        .or(library_concurrency)
        .unwrap_or_else(|| i64::try_from(default_concurrency).unwrap_or(i64::MAX));
    configured.max(1)
}

#[cfg(test)]
mod tests;
