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
        background_work::{BackgroundWorkStage, run_process_background_blocking},
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
        Database, FilesystemEntryMove, FilesystemEntryUpdate,
        MANIFEST_POSTPROCESSING_TARGET_PAGE_SIZE, ManifestDeltaBatchCommit,
        ManifestDiscoveryCommitResult, ManifestPostprocessingTargetPage, MetadataAutoMatchPolicy,
        NewEpisodeFile, NewFilesystemEntry, NewHierarchyItem, NewItemMetadataCompletenessResult,
        NewMediaItem, NewMediaSource, NewMovieFile, NewScanManifest, NewScanManifestDelta,
        NewScanManifestDiscoveryChunk, NewScanManifestEntry, NewScanManifestIndexedFile,
        NewScanManifestPositiveIndex, NewScanManifestRoot, NewScanManifestSeenFilesystemEntry,
        NewScanManifestUnresolvedFile, ReconciliationBatchCommit, StorageError,
        StoredEpisodeIdentityCandidate, StoredFilesystemEntry, StoredLibraryRoot,
        StoredReconciliationScanEntry, StoredScanJob, StoredScanJobPath,
        StoredScanLocalMetadataBackfillPage, StoredScanLocalMetadataBatch,
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

#[cfg(test)]
use jobs::local_metadata::{
    LOCAL_METADATA_COMPLETION_FALLBACK, LOCAL_METADATA_JOB_REFRESH_FALLBACK,
    LocalMetadataCompletenessTrigger, ScraperAvailabilityPreflight,
    metadata_auto_match_policy_for_scan_job, metadata_auto_match_policy_for_scan_job_snapshot,
    notify_local_metadata_images_waiter, resolve_local_metadata_auto_match_policy,
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
const LIBRARY_DELETION_SCAN_CANCEL_TIMEOUT: Duration = Duration::from_secs(30);
const SQLITE_SCAN_LOCK_RETRY_DELAYS_MS: [u64; 3] = [250, 750, 1_500];
const SCAN_LOCK_DATABASE_WAIT_BACKOFF_MS: [u64; 6] = [10, 20, 40, 80, 160, 250];

fn scan_lock_database_wait_backoff(retry_number: usize) -> Duration {
    let delay_index = retry_number.min(SCAN_LOCK_DATABASE_WAIT_BACKOFF_MS.len() - 1);
    Duration::from_millis(SCAN_LOCK_DATABASE_WAIT_BACKOFF_MS[delay_index])
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

const SCAN_FINGERPRINT_V2_PREFIX: &[u8; 9] = b"LUX-FP-2\0";

pub fn compute_scan_fingerprint_v2(
    relative_path: &str,
    size: i64,
    modified_at: i64,
    device: Option<u64>,
    inode: Option<u64>,
) -> Vec<u8> {
    let path_bytes = relative_path.as_bytes();
    let mut input = Vec::with_capacity(path_bytes.len().saturating_add(64));
    input.extend_from_slice(b"LUX-SCAN-FP-2-INPUT\0");
    input.extend_from_slice(&(path_bytes.len() as u64).to_be_bytes());
    input.extend_from_slice(path_bytes);
    input.extend_from_slice(b"FILE");
    input.push(u8::from(size >= 0));
    input.extend_from_slice(&size.to_be_bytes());
    input.push(u8::from(modified_at > 0));
    input.extend_from_slice(&modified_at.to_be_bytes());
    for value in [device, inode] {
        match value {
            Some(value) => {
                input.push(1);
                input.extend_from_slice(&value.to_be_bytes());
            }
            None => {
                input.push(0);
                input.extend_from_slice(&0_u64.to_be_bytes());
            }
        }
    }
    let digest = xxhash_rust::xxh3::xxh3_128_with_seed(&input, 0).to_be_bytes();
    let mut fingerprint = Vec::with_capacity(SCAN_FINGERPRINT_V2_PREFIX.len() + digest.len());
    fingerprint.extend_from_slice(SCAN_FINGERPRINT_V2_PREFIX);
    fingerprint.extend_from_slice(&digest);
    fingerprint
}

pub(super) async fn budgeted_file_fingerprint(
    path: &Path,
    relative_path: String,
    size: i64,
    modified_at: i64,
    device: Option<u64>,
    inode: Option<u64>,
) -> Result<Vec<u8>, ScannerError> {
    let display_path = path.to_owned();
    run_process_background_blocking(BackgroundWorkStage::Fingerprint, move || {
        compute_scan_fingerprint_v2(&relative_path, size, modified_at, device, inode)
    })
    .await
    .map_err(|source| ScannerError::Io {
        path: display_path,
        source: std::io::Error::other(source.to_string()),
    })
}

pub(crate) fn is_supported_scan_fingerprint(fingerprint: &[u8]) -> bool {
    fingerprint.len() == 32
        || fingerprint.len() == SCAN_FINGERPRINT_V2_PREFIX.len() + 16
            && fingerprint.starts_with(SCAN_FINGERPRINT_V2_PREFIX)
}

pub(crate) fn scan_fingerprint_matches_observation(
    baseline: &crate::storage::StoredFilesystemEntry,
    relative_path: &str,
    size: i64,
    modified_at: i64,
    device: Option<u64>,
    inode: Option<u64>,
) -> bool {
    let Some(fingerprint) = baseline.fingerprint.as_deref() else {
        return false;
    };
    if baseline.is_missing
        || baseline.entry_kind != "FILE"
        || baseline.relative_path != relative_path
        || !is_supported_scan_fingerprint(fingerprint)
    {
        return false;
    }

    let Some(device) = device else {
        return false;
    };
    let Some(inode) = inode else {
        return false;
    };
    if size < 0 || modified_at <= 0 {
        return false;
    }

    let stat_matches = baseline.size == size
        && baseline.modified_at == modified_at
        && baseline.device == i64::try_from(device).ok()
        && baseline.inode == i64::try_from(inode).ok();
    if stat_matches {
        return true;
    }

    fingerprint.len() == 32
        && fingerprint
            == compute_file_fingerprint(relative_path, size, modified_at, Some(device), Some(inode))
                .as_slice()
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
    let fingerprint_pending = entry_kind == "FILE";
    let fingerprint = if fingerprint_pending {
        Vec::new()
    } else {
        compute_file_fingerprint(&relative_path, size, modified_at, device, inode)
    };
    Ok(NewScanManifestEntry {
        relative_path,
        entry_kind: entry_kind.to_owned(),
        size,
        modified_at,
        device: device.and_then(|device| i64::try_from(device).ok()),
        inode: inode.and_then(|inode| i64::try_from(inode).ok()),
        fingerprint,
        fingerprint_pending,
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
        // Zero marks a timestamp that cannot safely participate in baseline reuse.
        i64::try_from(modified_at).unwrap_or(0)
    };
    let device = u64::try_from(metadata.st_dev).ok();
    let inode: u64 = metadata.st_ino;
    let fingerprint_pending = entry_kind == "FILE";
    let fingerprint = if fingerprint_pending {
        Vec::new()
    } else {
        compute_file_fingerprint(&relative_path, size, modified_at, device, Some(inode))
    };
    Ok(NewScanManifestEntry {
        relative_path,
        entry_kind: entry_kind.to_owned(),
        size,
        modified_at,
        device: device.and_then(|device| i64::try_from(device).ok()),
        inode: i64::try_from(inode).ok(),
        fingerprint,
        fingerprint_pending,
    })
}

#[derive(Clone, Debug)]
struct CurrentFileObservation {
    relative_path: String,
    size: i64,
    modified_at: i64,
    device: Option<u64>,
    inode: Option<u64>,
}

impl CurrentFileObservation {
    fn matches(&self, baseline: &crate::storage::StoredFilesystemEntry) -> bool {
        scan_fingerprint_matches_observation(
            baseline,
            &self.relative_path,
            self.size,
            self.modified_at,
            self.device,
            self.inode,
        )
    }
}

async fn current_file_observation(
    root_path: &Path,
    path: &Path,
) -> Result<CurrentFileObservation, ScannerError> {
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
    Ok(CurrentFileObservation {
        relative_path,
        size,
        modified_at,
        device,
        inode,
    })
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
