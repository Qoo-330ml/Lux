use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fmt,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use quick_xml::{events::Event, reader::Reader};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
use std::os::{
    fd::{AsRawFd, FromRawFd},
    unix::{ffi::OsStrExt, fs::OpenOptionsExt},
};
use tokio::{
    fs,
    sync::{Notify, OwnedSemaphorePermit, Semaphore, watch},
    task::{JoinHandle, JoinSet},
};
use uuid::Uuid;

use crate::{
    application::{
        admin_events::{AdminEventHub, AdminEventScope, UserEventHub},
        home::HomeService,
        library_covers::{AutoLibraryCoverResult, LibraryCoverService},
        media_matching::{
            MediaKind, clean_title, has_multi_part_marker, has_source_variant_marker,
            parse_media_name,
        },
        metadata::MetadataEnricher,
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
    observability::resources::ResourceMetrics,
    storage::{
        Database, FilesystemEntryMove, ManifestDeltaBatchCommit, ManifestDiscoveryCommitResult,
        ManifestPostprocessingTargetPage, NewEpisodeFile, NewFilesystemEntry, NewHierarchyItem,
        NewMediaItem, NewMediaSource, NewMovieFile, NewScanJobEvent, NewScanManifest,
        NewScanManifestDelta, NewScanManifestDiscoveryChunk, NewScanManifestEntry,
        NewScanManifestIndexedFile, NewScanManifestPositiveIndex, NewScanManifestRoot,
        NewScanManifestSeenFilesystemEntry, NewScanManifestSidecarEntry,
        NewScanManifestUnresolvedFile, ReconciliationBatchCommit, StorageError,
        StoredEpisodeIdentityCandidate, StoredFilesystemEntry, StoredLibraryRoot,
        StoredReconciliationScanEntry, StoredScanJob, StoredScanJobPath, StoredScanManifestDelta,
        StoredScanManifestFilesystemBaseline, is_lite_manifest_discovery,
        movie_parent_folder_identity,
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
const MAX_MANIFEST_STRM_TARGET_BYTES: usize = 1024 * 1024;

struct ManifestDirectoryBatch {
    child_directories: Vec<String>,
    entries: Vec<NewScanManifestEntry>,
    completed: bool,
    readdir_duration: Duration,
    stat_duration: Duration,
    readdir_entry_count: usize,
    stat_entry_count: usize,
}

fn record_manifest_scan_stage(
    phase: &'static str,
    started: Instant,
    units: u64,
    files: u64,
    directories: u64,
) {
    record_manifest_scan_stage_duration(phase, started.elapsed(), units, files, directories);
}

fn record_manifest_scan_stage_duration(
    phase: &'static str,
    duration: Duration,
    units: u64,
    files: u64,
    directories: u64,
) {
    if tracing::enabled!(target: "lux::scan_performance", tracing::Level::DEBUG) {
        tracing::debug!(
            target: "lux::scan_performance",
            phase,
            duration_us = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX),
            units,
            files,
            directories,
            "manifest scan stage timing"
        );
    }
}

fn record_manifest_scan_activity(active_preparation_tasks: usize, active_directory_readers: usize) {
    if tracing::enabled!(target: "lux::scan_performance", tracing::Level::DEBUG) {
        tracing::debug!(
            target: "lux::scan_performance",
            phase = "active_work",
            active_preparation_tasks = u64::try_from(active_preparation_tasks).unwrap_or(u64::MAX),
            active_directory_readers = u64::try_from(active_directory_readers).unwrap_or(u64::MAX),
            "manifest scan active work"
        );
    }
}

struct PendingManifestDirectoryChunk {
    relative_directory: String,
    root_observation: NewScanManifestEntry,
    directory_observation: NewScanManifestEntry,
    child_directories: Vec<String>,
    entries: Vec<NewScanManifestEntry>,
    completed_directory: Option<String>,
}

#[derive(Default)]
struct ManifestDiscoveryDirectoryResult {
    observed_file_count: usize,
    created_items: usize,
    child_directories: Vec<String>,
}

#[derive(Default)]
struct LiteManifestDiscoverySession {
    directories: VecDeque<(String, String)>,
}

enum PreparedManifestFile {
    Movie(NewMovieFile),
    Episode(NewEpisodeFile),
    Unresolved(NewScanManifestUnresolvedFile),
}

enum ManifestDeltaPreparation {
    Stable {
        file: Option<Box<PreparedManifestFile>>,
        sidecar_entry: Option<NewScanManifestSidecarEntry>,
    },
    Unstable {
        delta_id: Option<String>,
    },
    RootIdentityChanged {
        delta_id: Option<String>,
    },
}

struct ManifestPositiveIndexSeed {
    relative_path: String,
    delta_kind: String,
    base_filesystem_entry_id: Option<String>,
    base_fingerprint: Option<Vec<u8>>,
}

// The JoinSet bounds live values to scan concurrency; boxing would allocate once per indexed
// file on the 60k-file hot path.
#[allow(clippy::large_enum_variant)]
enum ManifestDiscoveryIndexPreparation {
    Indexed(NewScanManifestPositiveIndex),
    Unstable,
    RootIdentityChanged,
}

struct ManifestFilePreparationContext {
    scanner: LibraryScanner,
    root: StoredLibraryRoot,
    root_path: PathBuf,
    expected_root_device: Option<i64>,
    expected_root_inode: Option<i64>,
    verify_path_after_preparation: bool,
}

#[derive(Clone, Copy)]
struct ManifestRootDiscoveryContext<'a> {
    job_id: &'a str,
    manifest_id: &'a str,
    root: &'a StoredLibraryRoot,
    cancellation: &'a AtomicBool,
    stream_files_during_discovery: bool,
    library_kind: &'a str,
    preparation_concurrency: usize,
    expected_root_identity: Option<(i64, i64)>,
}

#[derive(Clone, Copy)]
struct ManifestPositiveIndexPreparationContext<'a> {
    root: &'a StoredLibraryRoot,
    relative_directory: &'a str,
    library_kind: &'a str,
    preparation_concurrency: usize,
    baselines: &'a HashMap<String, StoredScanManifestFilesystemBaseline>,
    expected_root_identity: Option<(i64, i64)>,
    expected_root_observation: &'a NewScanManifestEntry,
    expected_directory_observation: &'a NewScanManifestEntry,
    entries: &'a [NewScanManifestEntry],
    cancellation: &'a AtomicBool,
}

#[derive(Default)]
struct ManifestApplyPreparedFiles {
    movie_files: Vec<NewMovieFile>,
    episode_files: Vec<NewEpisodeFile>,
    unresolved_files: Vec<NewScanManifestUnresolvedFile>,
    sidecar_entries: Vec<NewScanManifestSidecarEntry>,
    unstable_delta_ids: Vec<String>,
    root_identity_lost: bool,
}

impl ManifestApplyPreparedFiles {
    fn record(&mut self, preparation: ManifestDeltaPreparation) {
        match preparation {
            ManifestDeltaPreparation::Stable {
                file,
                sidecar_entry,
                ..
            } => {
                if let Some(file) = file {
                    match *file {
                        PreparedManifestFile::Movie(file) => self.movie_files.push(file),
                        PreparedManifestFile::Episode(file) => self.episode_files.push(file),
                        PreparedManifestFile::Unresolved(file) => self.unresolved_files.push(file),
                    }
                }
                if let Some(sidecar_entry) = sidecar_entry {
                    self.sidecar_entries.push(sidecar_entry);
                }
            }
            ManifestDeltaPreparation::Unstable { delta_id } => {
                if let Some(delta_id) = delta_id {
                    self.unstable_delta_ids.push(delta_id);
                }
            }
            ManifestDeltaPreparation::RootIdentityChanged { delta_id } => {
                self.root_identity_lost = true;
                if let Some(delta_id) = delta_id {
                    self.unstable_delta_ids.push(delta_id);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum ManifestRemovalOutcome {
    Missing,
    Present,
    PathIoError,
    InvalidPath,
    RootIdentityChanged,
}

#[derive(Default)]
struct ManifestRemovalDecision {
    confirmed_missing_ids: Vec<String>,
    unstable_ids: Vec<String>,
    root_identity_lost: bool,
}

fn classify_manifest_removal_outcomes(
    outcomes: &[(String, ManifestRemovalOutcome)],
) -> ManifestRemovalDecision {
    let root_identity_lost = outcomes
        .iter()
        .any(|(_, outcome)| matches!(outcome, ManifestRemovalOutcome::RootIdentityChanged));
    let any_path_io_error = outcomes
        .iter()
        .any(|(_, outcome)| matches!(outcome, ManifestRemovalOutcome::PathIoError));
    if root_identity_lost || any_path_io_error {
        return ManifestRemovalDecision {
            confirmed_missing_ids: Vec::new(),
            unstable_ids: outcomes.iter().map(|(id, _)| id.clone()).collect(),
            root_identity_lost,
        };
    }

    let mut decision = ManifestRemovalDecision::default();
    for (id, outcome) in outcomes {
        match outcome {
            ManifestRemovalOutcome::Missing => decision.confirmed_missing_ids.push(id.clone()),
            ManifestRemovalOutcome::Present
            | ManifestRemovalOutcome::InvalidPath
            | ManifestRemovalOutcome::PathIoError
            | ManifestRemovalOutcome::RootIdentityChanged => {
                decision.unstable_ids.push(id.clone());
            }
        }
    }
    decision
}

impl PreparedManifestFile {
    fn matches_observation(&self, observation: &NewScanManifestEntry) -> bool {
        match self {
            Self::Movie(file) => {
                file.relative_path == observation.relative_path
                    && file.size == observation.size
                    && file.modified_at == observation.modified_at
                    && file.fingerprint == observation.fingerprint
            }
            Self::Episode(file) => {
                file.relative_path == observation.relative_path
                    && file.size == observation.size
                    && file.modified_at == observation.modified_at
                    && file.inode == observation.inode
                    && file.fingerprint == observation.fingerprint
            }
            Self::Unresolved(file) => {
                file.relative_path == observation.relative_path
                    && file.size == observation.size
                    && file.modified_at == observation.modified_at
                    && file.inode == observation.inode
                    && file.fingerprint == observation.fingerprint
            }
        }
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
struct ManifestDirectoryReader {
    root_path: PathBuf,
    relative_directory: String,
    _directory: std::fs::File,
    entries: *mut libc::DIR,
    root_observation: NewScanManifestEntry,
    root_observation_emitted: bool,
    directory_observation: NewScanManifestEntry,
}

// The DIR stream is exclusively owned and is only accessed by one blocking task at a time.
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
// SAFETY: the raw pointer is created by `fdopendir`, never copied, only dereferenced while
// the reader is moved by value into a blocking task, and closed exactly once by `Drop`.
unsafe impl Send for ManifestDirectoryReader {}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
fn open_manifest_directory_identity(
    root_path: &Path,
    relative_directory: &str,
) -> Result<(std::fs::File, NewScanManifestEntry, NewScanManifestEntry), ScannerError> {
    if !root_path.is_absolute() {
        return Err(ScannerError::InvalidRelativePath(
            root_path.to_string_lossy().into_owned(),
        ));
    }
    let display_path = root_path.join(relative_directory);
    let mut directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")
        .map_err(|source| ScannerError::Io {
            path: PathBuf::from("/"),
            source,
        })?;

    for component in root_path.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(ScannerError::InvalidRelativePath(
                relative_directory.to_owned(),
            ));
        };
        directory = open_manifest_directory_component(
            directory.as_raw_fd(),
            name,
            &display_path,
            relative_directory,
        )?;
    }

    let root_metadata = directory.metadata().map_err(|source| ScannerError::Io {
        path: root_path.to_owned(),
        source,
    })?;
    let root_observation =
        manifest_entry_observation(String::new(), "DIRECTORY", &root_metadata, root_path)?;

    for component in Path::new(relative_directory).components() {
        let Component::Normal(name) = component else {
            return Err(ScannerError::InvalidRelativePath(
                relative_directory.to_owned(),
            ));
        };
        directory = open_manifest_directory_component(
            directory.as_raw_fd(),
            name,
            &display_path,
            relative_directory,
        )?;
    }

    let metadata = directory.metadata().map_err(|source| ScannerError::Io {
        path: display_path.clone(),
        source,
    })?;
    let directory_observation = manifest_entry_observation(
        relative_directory.to_owned(),
        "DIRECTORY",
        &metadata,
        &display_path,
    )?;
    Ok((directory, root_observation, directory_observation))
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
impl ManifestDirectoryReader {
    fn open(root_path: &Path, relative_directory: &str) -> Result<Self, ScannerError> {
        let display_path = root_path.join(relative_directory);
        let (directory, root_observation, directory_observation) =
            open_manifest_directory_identity(root_path, relative_directory)?;
        // SAFETY: directory owns a valid descriptor, and fcntl does not take ownership.
        let entries_fd = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if entries_fd < 0 {
            return Err(ScannerError::Io {
                path: display_path,
                source: std::io::Error::last_os_error(),
            });
        }
        // SAFETY: entries_fd is a fresh valid descriptor for a directory; fdopendir takes
        // ownership of it, leaving `directory` available for openat/fstatat calls.
        let entries = unsafe { libc::fdopendir(entries_fd) };
        if entries.is_null() {
            let source = std::io::Error::last_os_error();
            // SAFETY: fdopendir failed, so ownership of entries_fd did not transfer.
            unsafe { libc::close(entries_fd) };
            return Err(ScannerError::Io {
                path: display_path,
                source,
            });
        }

        Ok(Self {
            root_path: root_path.to_owned(),
            relative_directory: relative_directory.to_owned(),
            _directory: directory,
            entries,
            root_observation,
            root_observation_emitted: false,
            directory_observation,
        })
    }

    fn next_batch(
        mut self,
        max_entries: usize,
    ) -> Result<(Self, ManifestDirectoryBatch), ScannerError> {
        let batch_size = max_entries.clamp(1, MANIFEST_STREAMED_ENTRY_BATCH_SIZE);
        let mut child_directories = Vec::with_capacity(batch_size);
        let mut observations = Vec::with_capacity(batch_size);
        let mut completed = false;
        let measure_stages =
            tracing::enabled!(target: "lux::scan_performance", tracing::Level::DEBUG);
        let mut readdir_duration = Duration::ZERO;
        let mut stat_duration = Duration::ZERO;
        let mut readdir_entry_count = 0_usize;
        let mut stat_entry_count = 0_usize;

        if self.relative_directory.is_empty() && !self.root_observation_emitted {
            observations.push(self.root_observation.clone());
            self.root_observation_emitted = true;
        }

        for _ in 0..batch_size {
            clear_manifest_errno();
            let readdir_started = measure_stages.then(Instant::now);
            // SAFETY: `entries` remains a live, exclusively owned DIR stream until Drop.
            let entry = unsafe { libc::readdir(self.entries) };
            if let Some(started) = readdir_started {
                readdir_duration = readdir_duration.saturating_add(started.elapsed());
            }
            readdir_entry_count = readdir_entry_count.saturating_add(1);
            if entry.is_null() {
                let source = std::io::Error::last_os_error();
                if source.raw_os_error().is_some_and(|code| code != 0) {
                    return Err(ScannerError::Io {
                        path: self.root_path.join(&self.relative_directory),
                        source,
                    });
                }
                completed = true;
                break;
            }
            // SAFETY: readdir returns a dirent whose d_name is NUL-terminated and remains
            // valid until the next readdir call; the name is copied immediately below.
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }
                .to_str()
                .map_err(|_| ScannerError::NonUtf8Path)?;
            if name == "." || name == ".." {
                continue;
            }
            let relative_path = Path::new(&self.relative_directory)
                .join(name)
                .to_str()
                .ok_or(ScannerError::NonUtf8Path)?
                .to_owned();
            let path = self.root_path.join(&relative_path);
            let name_c = std::ffi::CString::new(name)
                .map_err(|_| ScannerError::InvalidRelativePath(relative_path.clone()))?;
            let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
            let stat_started = measure_stages.then(Instant::now);
            // SAFETY: the directory descriptor is open, name_c is NUL-terminated, and stat
            // points to writable storage for the duration of the call.
            let stat_result = unsafe {
                libc::fstatat(
                    self._directory.as_raw_fd(),
                    name_c.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if let Some(started) = stat_started {
                stat_duration = stat_duration.saturating_add(started.elapsed());
            }
            stat_entry_count = stat_entry_count.saturating_add(1);
            if stat_result < 0 {
                let source = std::io::Error::last_os_error();
                if source.raw_os_error().is_some_and(|code| {
                    code == libc::ENOENT || code == libc::ENOTDIR || code == libc::ELOOP
                }) {
                    continue;
                }
                return Err(ScannerError::Io {
                    path: path.clone(),
                    source,
                });
            }
            // SAFETY: fstatat initialized the struct on success.
            let stat = unsafe { stat.assume_init() };
            let file_type = stat.st_mode & libc::S_IFMT;
            let is_directory = file_type == libc::S_IFDIR;
            let is_regular_file = file_type == libc::S_IFREG;
            if !(is_directory
                || is_regular_file
                    && (is_supported_movie_file(Path::new(name))
                        || is_supported_sidecar_file(Path::new(name))))
            {
                continue;
            }
            let entry_kind = if is_directory {
                child_directories.push(relative_path.clone());
                Some("DIRECTORY")
            } else if is_regular_file {
                Some("FILE")
            } else {
                None
            };
            if let Some(entry_kind) = entry_kind {
                observations.push(manifest_entry_observation_from_stat(
                    relative_path,
                    entry_kind,
                    &stat,
                    &path,
                )?);
            }
        }

        if completed && !self.directory_observation.relative_path.is_empty() {
            observations.push(self.directory_observation.clone());
        }
        Ok((
            self,
            ManifestDirectoryBatch {
                child_directories,
                entries: observations,
                completed,
                readdir_duration,
                stat_duration,
                readdir_entry_count,
                stat_entry_count,
            },
        ))
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn clear_manifest_errno() {
    // SAFETY: errno location is thread-local and readdir is called synchronously afterward.
    unsafe { *libc::__errno_location() = 0 };
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn clear_manifest_errno() {
    // SAFETY: errno location is thread-local and readdir is called synchronously afterward.
    unsafe { *libc::__error() = 0 };
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
impl Drop for ManifestDirectoryReader {
    fn drop(&mut self) {
        if !self.entries.is_null() {
            // SAFETY: this reader uniquely owns the stream returned by fdopendir.
            unsafe { libc::closedir(self.entries) };
            self.entries = std::ptr::null_mut();
        }
    }
}

async fn stat_manifest_relative_file(
    root_path: PathBuf,
    relative_path: String,
    expected_root_device: Option<i64>,
    expected_root_inode: Option<i64>,
) -> Result<Option<NewScanManifestEntry>, ScannerError> {
    let display_path = root_path.clone();
    tokio::task::spawn_blocking(move || {
        stat_manifest_relative_file_sync(
            &root_path,
            &relative_path,
            expected_root_device,
            expected_root_inode,
        )
    })
    .await
    .map_err(|source| ScannerError::Io {
        path: display_path,
        source: std::io::Error::other(source.to_string()),
    })?
}

async fn read_manifest_strm_target(
    root_path: PathBuf,
    relative_path: String,
    expected_root_device: Option<i64>,
    expected_root_inode: Option<i64>,
    expected_observation: NewScanManifestEntry,
) -> Result<StrmTarget, ScannerError> {
    let display_path = root_path.join(&relative_path);
    tokio::task::spawn_blocking(move || {
        read_manifest_strm_target_sync(
            &root_path,
            &relative_path,
            expected_root_device,
            expected_root_inode,
            &expected_observation,
        )
    })
    .await
    .map_err(|source| ScannerError::Io {
        path: display_path.clone(),
        source: std::io::Error::other(source.to_string()),
    })?
}

async fn stat_manifest_root(root_path: PathBuf) -> Result<NewScanManifestEntry, ScannerError> {
    let display_path = root_path.clone();
    tokio::task::spawn_blocking(move || stat_manifest_root_sync(&root_path))
        .await
        .map_err(|source| ScannerError::Io {
            path: display_path,
            source: std::io::Error::other(source.to_string()),
        })?
}

fn stat_manifest_root_sync(root_path: &Path) -> Result<NewScanManifestEntry, ScannerError> {
    ManifestDirectoryReader::open(root_path, "").map(|reader| reader.root_observation.clone())
}

fn manifest_root_identity_matches(
    expected_device: Option<i64>,
    expected_inode: Option<i64>,
    observed: &NewScanManifestEntry,
) -> bool {
    match (expected_device, expected_inode) {
        (Some(device), Some(inode)) => {
            observed.device == Some(device) && observed.inode == Some(inode)
        }
        // Without a stable root identity, a replacement mount or directory cannot be
        // distinguished from the original root. Never authorize reconciliation deletes.
        (None, None) => false,
        _ => false,
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
fn stat_manifest_relative_file_sync(
    root_path: &Path,
    relative_path: &str,
    expected_root_device: Option<i64>,
    expected_root_inode: Option<i64>,
) -> Result<Option<NewScanManifestEntry>, ScannerError> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ScannerError::InvalidRelativePath(relative_path.to_owned()));
    }
    let Some(file_name) = relative.file_name() else {
        return Err(ScannerError::InvalidRelativePath(relative_path.to_owned()));
    };
    let parent = relative.parent().and_then(Path::to_str).unwrap_or_default();
    let reader = match ManifestDirectoryReader::open(root_path, parent) {
        Ok(reader) => reader,
        Err(ScannerError::Io { source, .. })
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            let root_observation = stat_manifest_root_sync(root_path)?;
            if manifest_root_identity_matches(
                expected_root_device,
                expected_root_inode,
                &root_observation,
            ) {
                return Ok(None);
            }
            return Err(ScannerError::RootIdentityChanged(root_path.to_owned()));
        }
        Err(error) => return Err(error),
    };
    if !manifest_root_identity_matches(
        expected_root_device,
        expected_root_inode,
        &reader.root_observation,
    ) {
        return Err(ScannerError::RootIdentityChanged(root_path.to_owned()));
    }
    let file_name = std::ffi::CString::new(file_name.as_bytes())
        .map_err(|_| ScannerError::InvalidRelativePath(relative_path.to_owned()))?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: the parent descriptor is live and file_name/stat remain valid for the call.
    let result = unsafe {
        libc::fstatat(
            reader._directory.as_raw_fd(),
            file_name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result < 0 {
        let source = std::io::Error::last_os_error();
        if source.kind() == std::io::ErrorKind::NotFound {
            return Ok(None);
        }
        return Err(ScannerError::Io {
            path: root_path.join(relative_path),
            source,
        });
    }
    // SAFETY: fstatat initialized the struct on success.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(ScannerError::InvalidRelativePath(relative_path.to_owned()));
    }
    manifest_entry_observation_from_stat(
        relative_path.to_owned(),
        "FILE",
        &stat,
        &root_path.join(relative_path),
    )
    .map(Some)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn stat_manifest_relative_file_sync(
    root_path: &Path,
    relative_path: &str,
    expected_root_device: Option<i64>,
    expected_root_inode: Option<i64>,
) -> Result<Option<NewScanManifestEntry>, ScannerError> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ScannerError::InvalidRelativePath(relative_path.to_owned()));
    }
    let parent = relative.parent().and_then(Path::to_str).unwrap_or_default();
    let reader = match ManifestDirectoryReader::open(root_path, parent) {
        Ok(reader) => reader,
        Err(ScannerError::Io { source, .. })
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            let root_observation = stat_manifest_root_sync(root_path)?;
            if manifest_root_identity_matches(
                expected_root_device,
                expected_root_inode,
                &root_observation,
            ) {
                return Ok(None);
            }
            return Err(ScannerError::RootIdentityChanged(root_path.to_owned()));
        }
        Err(error) => return Err(error),
    };
    if !manifest_root_identity_matches(
        expected_root_device,
        expected_root_inode,
        &reader.root_observation,
    ) {
        return Err(ScannerError::RootIdentityChanged(root_path.to_owned()));
    }
    let path = root_path.join(relative);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ScannerError::Io { path, source }),
    };
    if !metadata.file_type().is_file() {
        return Err(ScannerError::InvalidRelativePath(relative_path.to_owned()));
    }
    manifest_entry_observation(relative_path.to_owned(), "FILE", &metadata, &path).map(Some)
}

fn manifest_directory_observation_matches(
    expected: &NewScanManifestEntry,
    observed: &NewScanManifestEntry,
) -> bool {
    expected.entry_kind == "DIRECTORY"
        && observed.entry_kind == "DIRECTORY"
        && expected.relative_path == observed.relative_path
        && expected.device == observed.device
        && expected.inode == observed.inode
        && expected.device.is_some()
        && expected.inode.is_some()
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn open_manifest_directory_for_final_check(
    root_path: &Path,
    expected_root_observation: &NewScanManifestEntry,
    expected_directory_observation: &NewScanManifestEntry,
) -> Result<Option<ManifestDirectoryReader>, ScannerError> {
    match ManifestDirectoryReader::open(root_path, &expected_directory_observation.relative_path) {
        Ok(reader) => {
            if !manifest_root_identity_matches(
                expected_root_observation.device,
                expected_root_observation.inode,
                &reader.root_observation,
            ) || !manifest_directory_observation_matches(
                expected_directory_observation,
                &reader.directory_observation,
            ) {
                return Err(ScannerError::RootIdentityChanged(root_path.to_owned()));
            }
            Ok(Some(reader))
        }
        Err(ScannerError::Io { source, .. })
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            let current_root = stat_manifest_root_sync(root_path)?;
            if manifest_root_identity_matches(
                expected_root_observation.device,
                expected_root_observation.inode,
                &current_root,
            ) {
                Ok(None)
            } else {
                Err(ScannerError::RootIdentityChanged(root_path.to_owned()))
            }
        }
        Err(ScannerError::InvalidRelativePath(_)) => {
            Err(ScannerError::RootIdentityChanged(root_path.to_owned()))
        }
        Err(error) => Err(error),
    }
}

async fn verify_manifest_directory_observation(
    root_path: PathBuf,
    expected_root_observation: NewScanManifestEntry,
    expected_directory_observation: NewScanManifestEntry,
) -> Result<(), ScannerError> {
    let display_path = root_path.clone();
    tokio::task::spawn_blocking(move || {
        verify_manifest_directory_observation_sync(
            &root_path,
            &expected_root_observation,
            &expected_directory_observation,
        )
    })
    .await
    .map_err(|source| ScannerError::Io {
        path: display_path,
        source: std::io::Error::other(source.to_string()),
    })?
}

async fn verify_manifest_directory_observations(
    root_path: PathBuf,
    observations: Vec<(NewScanManifestEntry, NewScanManifestEntry)>,
) -> Result<(), ScannerError> {
    let display_path = root_path.clone();
    let directory_count = observations.len();
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        for (expected_root_observation, expected_directory_observation) in observations {
            verify_manifest_directory_observation_sync(
                &root_path,
                &expected_root_observation,
                &expected_directory_observation,
            )?;
        }
        Ok(())
    })
    .await
    .map_err(|source| ScannerError::Io {
        path: display_path,
        source: std::io::Error::other(source.to_string()),
    })?;
    record_manifest_scan_stage(
        "directory_identity_recheck",
        started,
        u64::try_from(directory_count).unwrap_or(u64::MAX),
        0,
        u64::try_from(directory_count).unwrap_or(u64::MAX),
    );
    result
}

fn verify_manifest_directory_observation_sync(
    root_path: &Path,
    expected_root_observation: &NewScanManifestEntry,
    expected_directory_observation: &NewScanManifestEntry,
) -> Result<(), ScannerError> {
    let canonical_root = std::fs::canonicalize(root_path).map_err(|source| ScannerError::Io {
        path: root_path.to_owned(),
        source,
    })?;
    let root_metadata =
        std::fs::symlink_metadata(root_path).map_err(|source| ScannerError::Io {
            path: root_path.to_owned(),
            source,
        })?;
    if canonical_root != root_path || !root_metadata.is_dir() {
        return Err(ScannerError::RootIdentityChanged(root_path.to_owned()));
    }
    let current_root =
        manifest_entry_observation(String::new(), "DIRECTORY", &root_metadata, root_path)?;
    if !manifest_root_identity_matches(
        expected_root_observation.device,
        expected_root_observation.inode,
        &current_root,
    ) {
        return Err(ScannerError::RootIdentityChanged(root_path.to_owned()));
    }

    let directory_path = root_path.join(&expected_directory_observation.relative_path);
    let canonical_directory =
        std::fs::canonicalize(&directory_path).map_err(|source| ScannerError::Io {
            path: directory_path.clone(),
            source,
        })?;
    let directory_metadata =
        std::fs::symlink_metadata(&directory_path).map_err(|source| ScannerError::Io {
            path: directory_path.clone(),
            source,
        })?;
    if canonical_directory != directory_path || !directory_metadata.is_dir() {
        return Err(ScannerError::RootIdentityChanged(root_path.to_owned()));
    }
    let current_directory = manifest_entry_observation(
        expected_directory_observation.relative_path.clone(),
        "DIRECTORY",
        &directory_metadata,
        &directory_path,
    )?;
    if !manifest_directory_observation_matches(expected_directory_observation, &current_directory) {
        return Err(ScannerError::RootIdentityChanged(root_path.to_owned()));
    }
    Ok(())
}

async fn stat_manifest_directory_file_batch(
    root_path: PathBuf,
    expected_root_observation: NewScanManifestEntry,
    expected_directory_observation: NewScanManifestEntry,
    expected_files: Vec<NewScanManifestEntry>,
) -> Result<Vec<Option<NewScanManifestEntry>>, ScannerError> {
    let display_path = root_path.clone();
    let file_count = expected_files.len();
    let started = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        stat_manifest_directory_file_batch_sync(
            &root_path,
            &expected_root_observation,
            &expected_directory_observation,
            &expected_files,
        )
    })
    .await
    .map_err(|source| ScannerError::Io {
        path: display_path,
        source: std::io::Error::other(source.to_string()),
    })?;
    record_manifest_scan_stage(
        "positive_file_recheck",
        started,
        u64::try_from(file_count).unwrap_or(u64::MAX),
        u64::try_from(file_count).unwrap_or(u64::MAX),
        0,
    );
    result
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
fn stat_manifest_directory_file_batch_sync(
    root_path: &Path,
    expected_root_observation: &NewScanManifestEntry,
    expected_directory_observation: &NewScanManifestEntry,
    expected_files: &[NewScanManifestEntry],
) -> Result<Vec<Option<NewScanManifestEntry>>, ScannerError> {
    let (directory, current_root, current_directory) = match open_manifest_directory_identity(
        root_path,
        &expected_directory_observation.relative_path,
    ) {
        Ok(observations) => observations,
        Err(ScannerError::InvalidRelativePath(_)) => {
            return Err(ScannerError::RootIdentityChanged(root_path.to_owned()));
        }
        Err(error) => return Err(error),
    };
    if !manifest_root_identity_matches(
        expected_root_observation.device,
        expected_root_observation.inode,
        &current_root,
    ) || !manifest_directory_observation_matches(
        expected_directory_observation,
        &current_directory,
    ) {
        return Err(ScannerError::RootIdentityChanged(root_path.to_owned()));
    }
    let current_files = stat_manifest_directory_files_from_handle(
        root_path,
        &directory,
        &expected_directory_observation.relative_path,
        expected_files,
    )?;
    drop(directory);
    verify_manifest_directory_observation_sync(
        root_path,
        expected_root_observation,
        expected_directory_observation,
    )?;
    Ok(current_files)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn stat_manifest_directory_file_batch_sync(
    root_path: &Path,
    expected_root_observation: &NewScanManifestEntry,
    expected_directory_observation: &NewScanManifestEntry,
    expected_files: &[NewScanManifestEntry],
) -> Result<Vec<Option<NewScanManifestEntry>>, ScannerError> {
    let Some(reader) = open_manifest_directory_for_final_check(
        root_path,
        expected_root_observation,
        expected_directory_observation,
    )?
    else {
        return Ok(vec![None; expected_files.len()]);
    };
    let current_files =
        stat_manifest_directory_files_from_reader(root_path, &reader, expected_files)?;
    drop(reader);
    match open_manifest_directory_for_final_check(
        root_path,
        expected_root_observation,
        expected_directory_observation,
    ) {
        Ok(Some(_reader)) => {}
        Ok(None) => return Ok(vec![None; expected_files.len()]),
        Err(error) => return Err(error),
    }
    Ok(current_files)
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
fn stat_manifest_directory_files_from_handle(
    root_path: &Path,
    directory: &std::fs::File,
    relative_directory: &str,
    expected_files: &[NewScanManifestEntry],
) -> Result<Vec<Option<NewScanManifestEntry>>, ScannerError> {
    let mut observations = Vec::with_capacity(expected_files.len());
    for expected in expected_files {
        let relative = Path::new(&expected.relative_path);
        if relative.parent().and_then(Path::to_str).unwrap_or_default() != relative_directory {
            return Err(ScannerError::InvalidRelativePath(
                expected.relative_path.clone(),
            ));
        }
        let Some(file_name) = relative.file_name() else {
            return Err(ScannerError::InvalidRelativePath(
                expected.relative_path.clone(),
            ));
        };
        let file_name = std::ffi::CString::new(file_name.as_bytes())
            .map_err(|_| ScannerError::InvalidRelativePath(expected.relative_path.clone()))?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: The secured parent directory descriptor and both C pointers stay live here.
        let result = unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                file_name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result < 0 {
            observations.push(None);
            continue;
        }
        // SAFETY: fstatat initialized the stat value on success.
        let stat = unsafe { stat.assume_init() };
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
            observations.push(None);
            continue;
        }
        observations.push(Some(manifest_entry_observation_from_stat(
            expected.relative_path.clone(),
            "FILE",
            &stat,
            &root_path.join(&expected.relative_path),
        )?));
    }
    Ok(observations)
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn stat_manifest_directory_files_from_reader(
    root_path: &Path,
    reader: &ManifestDirectoryReader,
    expected_files: &[NewScanManifestEntry],
) -> Result<Vec<Option<NewScanManifestEntry>>, ScannerError> {
    expected_files
        .iter()
        .map(|expected| {
            match stat_manifest_relative_file_sync(
                root_path,
                &expected.relative_path,
                reader.root_observation.device,
                reader.root_observation.inode,
            ) {
                Ok(observed) => Ok(observed),
                Err(ScannerError::RootIdentityChanged(path)) => {
                    Err(ScannerError::RootIdentityChanged(path))
                }
                Err(_) => Ok(None),
            }
        })
        .collect()
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
fn read_manifest_strm_target_sync(
    root_path: &Path,
    relative_path: &str,
    expected_root_device: Option<i64>,
    expected_root_inode: Option<i64>,
    expected_observation: &NewScanManifestEntry,
) -> Result<StrmTarget, ScannerError> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ScannerError::InvalidRelativePath(relative_path.to_owned()));
    }
    let Some(file_name) = relative.file_name() else {
        return Err(ScannerError::InvalidRelativePath(relative_path.to_owned()));
    };
    let parent = relative.parent().and_then(Path::to_str).unwrap_or_default();
    let reader = ManifestDirectoryReader::open(root_path, parent)?;
    if !manifest_root_identity_matches(
        expected_root_device,
        expected_root_inode,
        &reader.root_observation,
    ) {
        return Err(ScannerError::RootIdentityChanged(root_path.to_owned()));
    }
    let file_name = std::ffi::CString::new(file_name.as_bytes())
        .map_err(|_| ScannerError::InvalidRelativePath(relative_path.to_owned()))?;
    // O_NOFOLLOW closes the stat/read symlink race; O_NONBLOCK prevents a replaced FIFO
    // from blocking this bounded blocking worker before fstat can reject it.
    // SAFETY: the parent descriptor is live and file_name is a NUL-terminated C string.
    let descriptor = unsafe {
        libc::openat(
            reader._directory.as_raw_fd(),
            file_name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        return Err(ScannerError::Io {
            path: root_path.join(relative_path),
            source: std::io::Error::last_os_error(),
        });
    }
    // SAFETY: descriptor is newly opened and ownership transfers to File.
    let file = unsafe { std::fs::File::from_raw_fd(descriptor) };
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: the descriptor is live and stat is writable for fstat.
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } < 0 {
        return Err(ScannerError::Io {
            path: root_path.join(relative_path),
            source: std::io::Error::last_os_error(),
        });
    }
    // SAFETY: successful fstat initialized the struct.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(ScannerError::InvalidRelativePath(relative_path.to_owned()));
    }
    let file_size = usize::try_from(stat.st_size).map_err(|_| ScannerError::Io {
        path: root_path.join(relative_path),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "manifest STRM target has an invalid size",
        ),
    })?;
    if file_size > MAX_MANIFEST_STRM_TARGET_BYTES {
        return Err(ScannerError::Io {
            path: root_path.join(relative_path),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "manifest STRM target exceeds the size limit",
            ),
        });
    }
    let observed = manifest_entry_observation_from_stat(
        relative_path.to_owned(),
        "FILE",
        &stat,
        &root_path.join(relative_path),
    )?;
    if !manifest_file_observation_matches(expected_observation, &observed) {
        return Err(ScannerError::InvalidRelativePath(relative_path.to_owned()));
    }
    let mut contents = String::with_capacity(file_size);
    let bytes_read = file
        .take(u64::try_from(MAX_MANIFEST_STRM_TARGET_BYTES.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_string(&mut contents)
        .map_err(|source| ScannerError::Io {
            path: root_path.join(relative_path),
            source,
        })?;
    if bytes_read > MAX_MANIFEST_STRM_TARGET_BYTES {
        return Err(ScannerError::Io {
            path: root_path.join(relative_path),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "manifest STRM target exceeds the size limit",
            ),
        });
    }
    Ok(classify_strm_target(&contents))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
fn read_manifest_strm_target_sync(
    _root_path: &Path,
    relative_path: &str,
    _expected_root_device: Option<i64>,
    _expected_root_inode: Option<i64>,
    _expected_observation: &NewScanManifestEntry,
) -> Result<StrmTarget, ScannerError> {
    Err(ScannerError::InvalidRelativePath(relative_path.to_owned()))
}

fn manifest_file_observation_matches(
    expected: &NewScanManifestEntry,
    observed: &NewScanManifestEntry,
) -> bool {
    expected.relative_path == observed.relative_path
        && expected.entry_kind == observed.entry_kind
        && expected.size == observed.size
        && expected.modified_at == observed.modified_at
        && expected.device == observed.device
        && expected.inode == observed.inode
        && expected.fingerprint == observed.fingerprint
}

fn manifest_discovery_index_from_preparation(
    seed: ManifestPositiveIndexSeed,
    preparation: ManifestDeltaPreparation,
) -> ManifestDiscoveryIndexPreparation {
    match preparation {
        ManifestDeltaPreparation::Stable {
            file,
            sidecar_entry,
            ..
        } => {
            let file = if let Some(file) = file {
                match *file {
                    PreparedManifestFile::Movie(file) => NewScanManifestIndexedFile::Movie(file),
                    PreparedManifestFile::Episode(file) => {
                        NewScanManifestIndexedFile::Episode(file)
                    }
                    PreparedManifestFile::Unresolved(file) => {
                        NewScanManifestIndexedFile::Unresolved(file)
                    }
                }
            } else if let Some(sidecar_entry) = sidecar_entry {
                NewScanManifestIndexedFile::Sidecar(sidecar_entry)
            } else {
                let filesystem_entry_id = seed
                    .base_filesystem_entry_id
                    .clone()
                    .unwrap_or_else(|| FilesystemEntryId::new().to_string());
                NewScanManifestIndexedFile::Sidecar(NewScanManifestSidecarEntry {
                    filesystem_entry_id,
                    relative_path: seed.relative_path.clone(),
                })
            };
            ManifestDiscoveryIndexPreparation::Indexed(NewScanManifestPositiveIndex {
                relative_path: seed.relative_path,
                delta_kind: seed.delta_kind,
                base_filesystem_entry_id: seed.base_filesystem_entry_id,
                base_fingerprint: seed.base_fingerprint,
                file,
            })
        }
        ManifestDeltaPreparation::Unstable { .. } => ManifestDiscoveryIndexPreparation::Unstable,
        ManifestDeltaPreparation::RootIdentityChanged { .. } => {
            ManifestDiscoveryIndexPreparation::RootIdentityChanged
        }
    }
}

async fn prepare_manifest_delta(
    context: ManifestFilePreparationContext,
    delta: StoredScanManifestDelta,
    classification: Option<MixedClassification>,
) -> ManifestDeltaPreparation {
    let StoredScanManifestDelta {
        id,
        relative_path,
        delta_kind,
        entry_kind,
        size,
        modified_at,
        device,
        inode,
        fingerprint,
        ..
    } = delta;
    let delta_id = Some(id);
    let (Some(entry_kind), Some(size), Some(modified_at), Some(fingerprint)) =
        (entry_kind, size, modified_at, fingerprint)
    else {
        return ManifestDeltaPreparation::Unstable { delta_id };
    };
    let observed = NewScanManifestEntry {
        relative_path,
        entry_kind,
        size,
        modified_at,
        device,
        inode,
        fingerprint,
    };
    prepare_manifest_observation(
        context,
        observed,
        delta_kind == "ADD",
        delta_id,
        classification,
    )
    .await
}

async fn prepare_manifest_observation(
    context: ManifestFilePreparationContext,
    observed: NewScanManifestEntry,
    is_add: bool,
    delta_id: Option<String>,
    classification: Option<MixedClassification>,
) -> ManifestDeltaPreparation {
    let ManifestFilePreparationContext {
        scanner,
        root,
        root_path,
        expected_root_device,
        expected_root_inode,
        verify_path_after_preparation,
    } = context;
    if observed.entry_kind != "FILE" {
        return ManifestDeltaPreparation::Unstable { delta_id };
    }
    let path = root_path.join(&observed.relative_path);
    let is_media = is_supported_movie_file(Path::new(&observed.relative_path));
    let is_sidecar = is_supported_sidecar_file(Path::new(&observed.relative_path));
    if !is_media && !is_sidecar {
        return ManifestDeltaPreparation::Unstable { delta_id };
    }
    let prepared_file = if is_media {
        let Some(classification) = classification else {
            return ManifestDeltaPreparation::Unstable { delta_id };
        };
        let manifest_strm_target = if is_strm_file(&path) {
            match read_manifest_strm_target(
                root_path.clone(),
                observed.relative_path.clone(),
                expected_root_device,
                expected_root_inode,
                observed.clone(),
            )
            .await
            {
                Ok(target) => Some(target),
                Err(ScannerError::RootIdentityChanged(_)) => {
                    return ManifestDeltaPreparation::RootIdentityChanged { delta_id };
                }
                Err(_) => return ManifestDeltaPreparation::Unstable { delta_id },
            }
        } else {
            None
        };
        let prepared = match classification {
            MixedClassification::Movie => scanner
                .prepare_manifest_movie_file(&path, &observed, manifest_strm_target)
                .await
                .map(|file| file.map(PreparedManifestFile::Movie)),
            MixedClassification::Episode => scanner
                .prepare_manifest_episode_file(&root.id, &path, &observed, manifest_strm_target)
                .await
                .map(|file| file.map(PreparedManifestFile::Episode)),
            MixedClassification::Unresolved => scanner
                .prepare_manifest_unresolved_file(&root, &path, &observed, manifest_strm_target)
                .await
                .map(|file| Some(PreparedManifestFile::Unresolved(file))),
        };
        match prepared {
            Ok(Some(file)) if file.matches_observation(&observed) => Some(file),
            Ok(_) | Err(_) => return ManifestDeltaPreparation::Unstable { delta_id },
        }
    } else {
        None
    };
    if !verify_path_after_preparation {
        let sidecar_entry = (is_sidecar && is_add).then(|| NewScanManifestSidecarEntry {
            filesystem_entry_id: FilesystemEntryId::new().to_string(),
            relative_path: observed.relative_path,
        });
        return ManifestDeltaPreparation::Stable {
            file: prepared_file.map(Box::new),
            sidecar_entry,
        };
    }
    match stat_manifest_relative_file(
        root_path,
        observed.relative_path.clone(),
        expected_root_device,
        expected_root_inode,
    )
    .await
    {
        Ok(Some(current)) if manifest_file_observation_matches(&observed, &current) => {
            let sidecar_entry = (is_sidecar && is_add).then(|| NewScanManifestSidecarEntry {
                filesystem_entry_id: FilesystemEntryId::new().to_string(),
                relative_path: observed.relative_path,
            });
            ManifestDeltaPreparation::Stable {
                file: prepared_file.map(Box::new),
                sidecar_entry,
            }
        }
        Err(ScannerError::RootIdentityChanged(_)) => {
            ManifestDeltaPreparation::RootIdentityChanged { delta_id }
        }
        Ok(_) | Err(_) => ManifestDeltaPreparation::Unstable { delta_id },
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
fn open_manifest_directory_component(
    parent_descriptor: std::os::fd::RawFd,
    name: &std::ffi::OsStr,
    display_path: &Path,
    relative_directory: &str,
) -> Result<std::fs::File, ScannerError> {
    let name_c = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| ScannerError::InvalidRelativePath(relative_directory.to_owned()))?;
    // SAFETY: parent_descriptor remains owned by the caller and name_c is NUL-terminated.
    let descriptor = unsafe {
        libc::openat(
            parent_descriptor,
            name_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        let source = std::io::Error::last_os_error();
        if source.raw_os_error() == Some(libc::ELOOP) {
            return Err(ScannerError::InvalidRelativePath(
                relative_directory.to_owned(),
            ));
        }
        return Err(ScannerError::Io {
            path: display_path.to_owned(),
            source,
        });
    }
    // SAFETY: descriptor is a newly opened directory and ownership transfers to File.
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
struct ManifestDirectoryReader {
    root_path: PathBuf,
    relative_directory: String,
    directory_path: PathBuf,
    entries: std::fs::ReadDir,
    root_observation: NewScanManifestEntry,
    root_observation_emitted: bool,
    directory_observation: NewScanManifestEntry,
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
impl ManifestDirectoryReader {
    fn open(root_path: &Path, relative_directory: &str) -> Result<Self, ScannerError> {
        let canonical_root_path =
            std::fs::canonicalize(root_path).map_err(|source| ScannerError::Io {
                path: root_path.to_owned(),
                source,
            })?;
        if canonical_root_path != root_path {
            return Err(ScannerError::InvalidRelativePath(
                root_path.to_string_lossy().into_owned(),
            ));
        }
        let root_metadata =
            std::fs::metadata(&canonical_root_path).map_err(|source| ScannerError::Io {
                path: canonical_root_path.clone(),
                source,
            })?;
        let root_observation = manifest_entry_observation(
            String::new(),
            "DIRECTORY",
            &root_metadata,
            &canonical_root_path,
        )?;
        let directory_path = root_path.join(relative_directory);
        let canonical_directory_path =
            std::fs::canonicalize(&directory_path).map_err(|source| ScannerError::Io {
                path: directory_path.clone(),
                source,
            })?;
        if !canonical_directory_path.starts_with(root_path)
            || canonical_directory_path != directory_path
        {
            return Err(ScannerError::InvalidRelativePath(
                relative_directory.to_owned(),
            ));
        }
        let metadata =
            std::fs::metadata(&canonical_directory_path).map_err(|source| ScannerError::Io {
                path: canonical_directory_path.clone(),
                source,
            })?;
        let directory_observation = manifest_entry_observation(
            relative_directory.to_owned(),
            "DIRECTORY",
            &metadata,
            &canonical_directory_path,
        )?;
        let entries =
            std::fs::read_dir(&canonical_directory_path).map_err(|source| ScannerError::Io {
                path: canonical_directory_path.clone(),
                source,
            })?;
        Ok(Self {
            root_path: root_path.to_owned(),
            relative_directory: relative_directory.to_owned(),
            directory_path: canonical_directory_path,
            entries,
            root_observation,
            root_observation_emitted: false,
            directory_observation,
        })
    }

    fn next_batch(
        mut self,
        max_entries: usize,
    ) -> Result<(Self, ManifestDirectoryBatch), ScannerError> {
        let batch_size = max_entries.clamp(1, MANIFEST_STREAMED_ENTRY_BATCH_SIZE);
        let mut child_directories = Vec::with_capacity(batch_size);
        let mut observations = Vec::with_capacity(batch_size);
        let mut completed = false;
        let measure_stages =
            tracing::enabled!(target: "lux::scan_performance", tracing::Level::DEBUG);
        let mut readdir_duration = Duration::ZERO;
        let mut stat_duration = Duration::ZERO;
        let mut readdir_entry_count = 0_usize;
        let mut stat_entry_count = 0_usize;
        if self.relative_directory.is_empty() && !self.root_observation_emitted {
            observations.push(self.root_observation.clone());
            self.root_observation_emitted = true;
        }
        for _ in 0..batch_size {
            let readdir_started = measure_stages.then(Instant::now);
            let Some(entry) = self.entries.next() else {
                if let Some(started) = readdir_started {
                    readdir_duration = readdir_duration.saturating_add(started.elapsed());
                }
                readdir_entry_count = readdir_entry_count.saturating_add(1);
                completed = true;
                break;
            };
            if let Some(started) = readdir_started {
                readdir_duration = readdir_duration.saturating_add(started.elapsed());
            }
            readdir_entry_count = readdir_entry_count.saturating_add(1);
            let entry = entry.map_err(|source| ScannerError::Io {
                path: self.directory_path.clone(),
                source,
            })?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|source| ScannerError::Io {
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
                .strip_prefix(&self.root_path)
                .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                .to_str()
                .ok_or(ScannerError::NonUtf8Path)?
                .to_owned();
            let stat_started = measure_stages.then(Instant::now);
            let metadata = entry.metadata().map_err(|source| ScannerError::Io {
                path: path.clone(),
                source,
            })?;
            if let Some(started) = stat_started {
                stat_duration = stat_duration.saturating_add(started.elapsed());
            }
            stat_entry_count = stat_entry_count.saturating_add(1);
            let entry_kind = if file_type.is_dir() {
                child_directories.push(relative_path.clone());
                "DIRECTORY"
            } else {
                "FILE"
            };
            observations.push(manifest_entry_observation(
                relative_path,
                entry_kind,
                &metadata,
                &path,
            )?);
        }
        if completed && !self.directory_observation.relative_path.is_empty() {
            observations.push(self.directory_observation.clone());
        }
        Ok((
            self,
            ManifestDirectoryBatch {
                child_directories,
                entries: observations,
                completed,
                readdir_duration,
                stat_duration,
                readdir_entry_count,
                stat_entry_count,
            },
        ))
    }
}
const MISSING_ENTRY_BATCH_SIZE: usize = 500;
const LOCAL_METADATA_IDLE_FALLBACK: Duration = Duration::from_secs(1);
const LOCAL_METADATA_BATCH_SIZE: usize = 16;

#[derive(Clone)]
pub struct LibraryScanner {
    database: Database,
    scan_concurrency: usize,
}

impl LibraryScanner {
    pub fn new(database: Database) -> Self {
        let scan_concurrency =
            usize::try_from(scan_concurrency_from_env().unwrap_or(DEFAULT_SCAN_CONCURRENCY))
                .unwrap_or(1)
                .max(1);
        Self {
            database,
            scan_concurrency,
        }
    }

    pub async fn repair_legacy_identity_keys(&self) -> Result<usize, ScannerError> {
        if self.database.identity_stability_repair_completed().await? {
            return Ok(0);
        }
        let candidates = self
            .database
            .list_episode_identity_repair_candidates()
            .await?;
        let mut repaired = 0;
        for candidate in candidates {
            if self.repair_identity_candidate(candidate).await? {
                repaired += 1;
            }
        }
        self.database
            .mark_identity_stability_repair_completed()
            .await?;
        Ok(repaired)
    }

    async fn repair_identity_candidate(
        &self,
        candidate: StoredEpisodeIdentityCandidate,
    ) -> Result<bool, ScannerError> {
        let Some(root) = self
            .database
            .find_library_root(&candidate.library_root_id)
            .await?
        else {
            return Ok(false);
        };
        let Some(file_name) = Path::new(&candidate.relative_path)
            .file_name()
            .and_then(|value| value.to_str())
        else {
            return Ok(false);
        };
        let Some(parsed) = parse_episode_filename(file_name) else {
            return Ok(false);
        };
        if let Ok(metadata) =
            fs::metadata(Path::new(&root.canonical_path).join(&candidate.relative_path)).await
        {
            let (_, inode) = file_identity(&metadata);
            if let Some(inode) = inode.and_then(|value| i64::try_from(value).ok()) {
                self.database
                    .update_filesystem_entry_inode(&candidate.filesystem_entry_id, Some(inode))
                    .await?;
            }
        }
        let hierarchy = episode_hierarchy(&candidate.relative_path, &parsed);
        let series_identity = format!("series:{}:{}", root.id, hierarchy.series_path);
        let season_identity = format!("{series_identity}:season:{}", hierarchy.season_number);
        let episode_identity = Self::episode_identity_key(&root, &hierarchy, &parsed);
        self.database
            .repair_episode_hierarchy_identities(
                &candidate.episode_id,
                &series_identity,
                &season_identity,
                &episode_identity,
            )
            .await
            .map_err(ScannerError::from)
    }

    pub async fn scan_movie_library(
        &self,
        library_id: LibraryId,
    ) -> Result<ScanReport, ScannerError> {
        let library_id_text = library_id.to_string();
        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_none()
        {
            return Err(ScannerError::LibraryNotFound);
        }

        let generation = Uuid::now_v7().to_string();
        let roots = self.database.list_library_roots(&library_id_text).await?;
        let mut report = ScanReport::default();
        for root in roots {
            let root_path = PathBuf::from(&root.canonical_path);
            let root_is_available = fs::metadata(&root_path)
                .await
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false);
            if !root_is_available {
                self.database
                    .update_library_root_availability(&root.id, false)
                    .await?;
                report.unavailable_roots += 1;
                continue;
            }
            if !root.is_available {
                self.database
                    .update_library_root_availability(&root.id, true)
                    .await?;
            }
            let mut walker = FileBatchWalker::new(&root_path);
            while let Some(files) = walker.next_batch(FILE_BATCH_SIZE).await? {
                report.merge(
                    self.scan_movie_file_batch(
                        &library_id_text,
                        &root,
                        &root_path,
                        &files,
                        &generation,
                    )
                    .await?,
                );
            }
            report.marked_missing += usize::try_from(
                self.database
                    .mark_missing_filesystem_entries(&root.id, &generation)
                    .await?,
            )
            .unwrap_or(usize::MAX);
        }
        Ok(report)
    }

    pub async fn scan_series_library(
        &self,
        library_id: LibraryId,
    ) -> Result<ScanReport, ScannerError> {
        let library_id_text = library_id.to_string();
        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_none()
        {
            return Err(ScannerError::LibraryNotFound);
        }
        let generation = Uuid::now_v7().to_string();
        let roots = self.database.list_library_roots(&library_id_text).await?;
        let mut report = ScanReport::default();
        let mut refreshed_series = HashSet::new();
        for root in roots {
            let root_path = PathBuf::from(&root.canonical_path);
            let root_is_available = fs::metadata(&root_path)
                .await
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false);
            if !root_is_available {
                self.database
                    .update_library_root_availability(&root.id, false)
                    .await?;
                report.unavailable_roots += 1;
                continue;
            }
            if !root.is_available {
                self.database
                    .update_library_root_availability(&root.id, true)
                    .await?;
            }
            let mut walker = FileBatchWalker::new(&root_path);
            while let Some(files) = walker.next_batch(FILE_BATCH_SIZE).await? {
                let relative_paths = files
                    .iter()
                    .map(|path| {
                        path.strip_prefix(&root_path)
                            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                            .to_str()
                            .map(str::to_owned)
                            .ok_or(ScannerError::NonUtf8Path)
                    })
                    .collect::<Result<Vec<_>, ScannerError>>()?;
                let existing_entries = self
                    .database
                    .list_filesystem_entries_for_paths(&root.id, &relative_paths)
                    .await?;
                let mut seen_entry_ids = Vec::with_capacity(files.len());
                let mut new_paths = Vec::new();
                let quick_results = self
                    .scan_episode_files_if_unchanged(
                        &root,
                        &root_path,
                        &files,
                        &existing_entries,
                        self.scan_concurrency,
                    )
                    .await?;
                for (path, quick_result) in files.into_iter().zip(quick_results) {
                    if let Some((entry_id, quick_report, provider_update)) = quick_result {
                        if let Some((series_identity, provider_ids_json)) = provider_update
                            && refreshed_series.insert(series_identity.clone())
                        {
                            self.database
                                .update_local_provider_ids_for_identity_if_empty(
                                    &series_identity,
                                    &provider_ids_json,
                                )
                                .await?;
                        }
                        seen_entry_ids.push(entry_id);
                        report.merge(quick_report);
                        continue;
                    }
                    let relative_path = path
                        .strip_prefix(&root_path)
                        .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                        .to_str()
                        .ok_or(ScannerError::NonUtf8Path)?;
                    if !existing_entries.contains_key(relative_path) {
                        let requires_regular_scan = self
                            .file_has_moved_entry(&library_id_text, &root, &root_path, &path)
                            .await?
                            || self
                                .episode_path_has_legacy_identity(&root, &root_path, &path)
                                .await?;
                        if requires_regular_scan {
                            report.merge(
                                self.scan_episode_file_with_provider_cache(
                                    &library_id_text,
                                    &root,
                                    &root_path,
                                    &path,
                                    &generation,
                                    Some(&mut refreshed_series),
                                )
                                .await?,
                            );
                        } else {
                            new_paths.push(path);
                        }
                        continue;
                    }
                    report.merge(
                        self.scan_episode_file_with_provider_cache(
                            &library_id_text,
                            &root,
                            &root_path,
                            &path,
                            &generation,
                            Some(&mut refreshed_series),
                        )
                        .await?,
                    );
                }
                let new_episode_files = self
                    .prepare_new_episode_files(&root, &root_path, &new_paths)
                    .await?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                if !new_episode_files.is_empty() {
                    let file_count = new_episode_files.len();
                    report.created_items += self
                        .database
                        .insert_episode_files_batch(
                            &library_id_text,
                            &root.id,
                            &generation,
                            &new_episode_files,
                        )
                        .await?;
                    report.discovered_files += file_count;
                    report.created_sources += file_count;
                }
                self.database
                    .mark_filesystem_entries_seen_batch(&seen_entry_ids, &generation)
                    .await?;
            }
            report.marked_missing += usize::try_from(
                self.database
                    .mark_missing_filesystem_entries(&root.id, &generation)
                    .await?,
            )
            .unwrap_or(usize::MAX);
        }
        Ok(report)
    }

    pub async fn scan_mixed_library(
        &self,
        library_id: LibraryId,
    ) -> Result<ScanReport, ScannerError> {
        let library_id_text = library_id.to_string();
        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_none()
        {
            return Err(ScannerError::LibraryNotFound);
        }
        let generation = Uuid::now_v7().to_string();
        let roots = self.database.list_library_roots(&library_id_text).await?;
        let mut report = ScanReport::default();
        let mut refreshed_series = HashSet::new();
        for root in roots {
            let root_path = PathBuf::from(&root.canonical_path);
            let root_is_available = fs::metadata(&root_path)
                .await
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false);
            if !root_is_available {
                self.database
                    .update_library_root_availability(&root.id, false)
                    .await?;
                report.unavailable_roots += 1;
                continue;
            }
            if !root.is_available {
                self.database
                    .update_library_root_availability(&root.id, true)
                    .await?;
            }
            let mut walker = FileBatchWalker::new(&root_path);
            while let Some(files) = walker.next_batch(FILE_BATCH_SIZE).await? {
                let relative_paths = files
                    .iter()
                    .map(|path| {
                        path.strip_prefix(&root_path)
                            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                            .to_str()
                            .map(str::to_owned)
                            .ok_or(ScannerError::NonUtf8Path)
                    })
                    .collect::<Result<Vec<_>, ScannerError>>()?;
                let existing_entries = self
                    .database
                    .list_filesystem_entries_for_paths(&root.id, &relative_paths)
                    .await?;
                let mut seen_entry_ids = Vec::with_capacity(files.len());
                let mut new_movie_paths = Vec::new();
                let mut new_episode_paths = Vec::new();
                let mut classification_cache = MixedClassificationCache::default();
                for path in files {
                    let classification =
                        classify_mixed_file(&root_path, &path, &mut classification_cache).await;
                    let quick_report = match classification {
                        MixedClassification::Movie => {
                            let relative_path = path
                                .strip_prefix(&root_path)
                                .map_err(|error| {
                                    ScannerError::InvalidRelativePath(error.to_string())
                                })?
                                .to_str()
                                .ok_or(ScannerError::NonUtf8Path)?;
                            match existing_entries.get(relative_path) {
                                Some(existing_entry) => {
                                    self.scan_movie_file_if_unchanged(
                                        &root.id,
                                        &root_path,
                                        &path,
                                        existing_entry,
                                    )
                                    .await?
                                }
                                None => None,
                            }
                        }
                        MixedClassification::Episode => {
                            self.scan_episode_file_if_unchanged(
                                &root,
                                &root_path,
                                &path,
                                &existing_entries,
                            )
                            .await?
                        }
                        MixedClassification::Unresolved => {
                            self.scan_unresolved_file_if_unchanged(
                                &root,
                                &root_path,
                                &path,
                                &existing_entries,
                            )
                            .await?
                        }
                    };
                    if let Some((entry_id, quick_report)) = quick_report {
                        seen_entry_ids.push(entry_id);
                        report.merge(quick_report);
                        continue;
                    }
                    let relative_path = path
                        .strip_prefix(&root_path)
                        .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                        .to_str()
                        .ok_or(ScannerError::NonUtf8Path)?;
                    if !existing_entries.contains_key(relative_path) {
                        let requires_regular_scan = match classification {
                            MixedClassification::Movie => {
                                self.file_has_moved_entry(
                                    &library_id_text,
                                    &root,
                                    &root_path,
                                    &path,
                                )
                                .await?
                            }
                            MixedClassification::Episode => {
                                self.file_has_moved_entry(
                                    &library_id_text,
                                    &root,
                                    &root_path,
                                    &path,
                                )
                                .await?
                                    || self
                                        .episode_path_has_legacy_identity(&root, &root_path, &path)
                                        .await?
                            }
                            MixedClassification::Unresolved => true,
                        };
                        if !requires_regular_scan {
                            let batched = match classification {
                                MixedClassification::Movie => {
                                    new_movie_paths.push(path.clone());
                                    true
                                }
                                MixedClassification::Episode => {
                                    new_episode_paths.push(path.clone());
                                    true
                                }
                                MixedClassification::Unresolved => false,
                            };
                            if batched {
                                continue;
                            }
                        }
                    }
                    let result = match classification {
                        MixedClassification::Movie => {
                            self.scan_movie_file(
                                &library_id_text,
                                &root,
                                &root_path,
                                &path,
                                &generation,
                            )
                            .await?
                        }
                        MixedClassification::Episode => {
                            self.scan_episode_file_with_provider_cache(
                                &library_id_text,
                                &root,
                                &root_path,
                                &path,
                                &generation,
                                Some(&mut refreshed_series),
                            )
                            .await?
                        }
                        MixedClassification::Unresolved => {
                            self.scan_unresolved_file(
                                &library_id_text,
                                &root,
                                &root_path,
                                &path,
                                &generation,
                            )
                            .await?
                        }
                    };
                    report.merge(result);
                }
                let new_movie_files = self
                    .prepare_new_movie_files_with_concurrency(
                        &root_path,
                        &new_movie_paths,
                        self.scan_concurrency,
                    )
                    .await?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                if !new_movie_files.is_empty() {
                    let file_count = new_movie_files.len();
                    report.created_items += self
                        .database
                        .insert_movie_files_batch(
                            &library_id_text,
                            &root.id,
                            &generation,
                            &new_movie_files,
                        )
                        .await?;
                    report.discovered_files += file_count;
                    report.created_sources += file_count;
                }
                let new_episode_files = self
                    .prepare_new_episode_files_with_concurrency(
                        &root,
                        &root_path,
                        &new_episode_paths,
                        self.scan_concurrency,
                    )
                    .await?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                if !new_episode_files.is_empty() {
                    let file_count = new_episode_files.len();
                    report.created_items += self
                        .database
                        .insert_episode_files_batch(
                            &library_id_text,
                            &root.id,
                            &generation,
                            &new_episode_files,
                        )
                        .await?;
                    report.discovered_files += file_count;
                    report.created_sources += file_count;
                }
                self.database
                    .mark_filesystem_entries_seen_batch(&seen_entry_ids, &generation)
                    .await?;
            }
            report.marked_missing += usize::try_from(
                self.database
                    .mark_missing_filesystem_entries(&root.id, &generation)
                    .await?,
            )
            .unwrap_or(usize::MAX);
        }
        Ok(report)
    }

    async fn scan_episode_file(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        generation: &str,
    ) -> Result<ScanReport, ScannerError> {
        self.scan_episode_file_with_provider_cache(
            library_id_text,
            root,
            root_path,
            path,
            generation,
            None,
        )
        .await
    }

    async fn scan_episode_file_with_provider_cache(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        generation: &str,
        mut refreshed_series: Option<&mut HashSet<String>>,
    ) -> Result<ScanReport, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(ScanReport::default());
        };
        let Some(parsed) = parse_episode_filename(file_name) else {
            return Ok(ScanReport::default());
        };
        let is_strm = is_strm_file(path);
        let strm_target = if is_strm {
            Some(read_strm_target(path).await?)
        } else {
            None
        };
        let external_url = strm_target
            .as_ref()
            .and_then(|target| target.value.as_deref());
        let strm_target_kind = strm_target.as_ref().map(strm_target_kind_name);
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
        let fingerprint =
            compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
        let existing_entry = self
            .database
            .find_filesystem_entry(&root.id, &relative_path)
            .await?;
        let mut existing_entry = existing_entry;
        let inode = inode.and_then(|value| i64::try_from(value).ok());
        if existing_entry.is_none()
            && let Some(inode) = inode
            && let Some(entry) = self
                .database
                .find_filesystem_entry_by_inode(library_id_text, &root.id, inode, &relative_path)
                .await?
        {
            self.database
                .move_filesystem_entry(FilesystemEntryMove {
                    entry_id: &entry.id,
                    library_root_id: &root.id,
                    relative_path: &relative_path,
                    size,
                    modified_at,
                    inode: Some(inode),
                    fingerprint: &fingerprint,
                    generation,
                })
                .await?;
            existing_entry = Some(entry);
        }
        let hierarchy = episode_hierarchy(&relative_path, &parsed);
        let series_identity = format!("series:{}:{}", root.id, hierarchy.series_path);
        let season_identity = format!("{series_identity}:season:{}", hierarchy.season_number);
        let episode_identity = Self::episode_identity_key(root, &hierarchy, &parsed);
        if let Some(existing_item_id) = existing_entry
            .as_ref()
            .and_then(|entry| entry.item_id.as_deref())
            && existing_entry
                .as_ref()
                .and_then(|entry| entry.item_identity_key.as_deref())
                != Some(episode_identity.as_str())
        {
            self.database
                .repair_episode_hierarchy_identities(
                    existing_item_id,
                    &series_identity,
                    &season_identity,
                    &episode_identity,
                )
                .await?;
        }
        let fingerprint_unchanged = existing_entry
            .as_ref()
            .is_some_and(|entry| entry.fingerprint.as_deref() == Some(fingerprint.as_slice()));
        let episode_is_current = fingerprint_unchanged
            && existing_entry
                .as_ref()
                .and_then(|entry| entry.item_identity_key.as_deref())
                == Some(episode_identity.as_str());
        let should_refresh_series_provider_ids = episode_is_current
            && !hierarchy.provider_ids.is_empty()
            && existing_entry.as_ref().is_some_and(|entry| {
                entry
                    .series_provider_ids_json
                    .as_deref()
                    .is_none_or(|value| value.is_empty() || value == "{}")
            })
            && refreshed_series
                .as_ref()
                .is_none_or(|series| !series.contains(&series_identity));
        if should_refresh_series_provider_ids {
            let series_provider_ids_json = provider_ids_json(&hierarchy.provider_ids);
            if let Some(series_provider_ids_json) = series_provider_ids_json.as_deref() {
                self.database
                    .update_local_provider_ids_for_identity_if_empty(
                        &series_identity,
                        series_provider_ids_json,
                    )
                    .await?;
            }
            if let Some(series) = refreshed_series.as_mut() {
                series.insert(series_identity.clone());
            }
        }
        if episode_is_current && let Some(existing_entry) = existing_entry.as_ref() {
            if is_strm {
                self.database
                    .update_media_source_strm_target(
                        &existing_entry.id,
                        strm_target_kind,
                        external_url,
                    )
                    .await?;
            }
            self.database
                .mark_filesystem_entry_seen(&existing_entry.id, generation)
                .await?;
            self.database
                .update_filesystem_entry_inode(&existing_entry.id, inode)
                .await?;
            return Ok(ScanReport {
                discovered_files: 1,
                skipped_files: 1,
                ..ScanReport::default()
            });
        }

        let ensured = self
            .ensure_episode_hierarchy(library_id_text, root, &relative_path, &parsed, &hierarchy)
            .await?;
        if let Some(existing_entry) = existing_entry {
            let hierarchy_changed = self
                .database
                .reassign_media_source_item(&existing_entry.id, &ensured.episode_id)
                .await?;
            self.database
                .update_media_source_variant_labels(
                    &existing_entry.id,
                    parsed.edition_name.as_deref(),
                    parsed.quality_label.as_deref(),
                )
                .await?;
            if fingerprint_unchanged {
                if is_strm {
                    self.database
                        .update_media_source_strm_target(
                            &existing_entry.id,
                            strm_target_kind,
                            external_url,
                        )
                        .await?;
                }
                self.database
                    .mark_filesystem_entry_seen(&existing_entry.id, generation)
                    .await?;
                self.database
                    .update_filesystem_entry_inode(&existing_entry.id, inode)
                    .await?;
                return Ok(ScanReport {
                    discovered_files: 1,
                    created_items: ensured.created_items,
                    changed_files: usize::from(hierarchy_changed),
                    skipped_files: 1,
                    ..ScanReport::default()
                });
            }
            self.database
                .update_filesystem_entry(
                    &existing_entry.id,
                    size,
                    modified_at,
                    &fingerprint,
                    generation,
                )
                .await?;
            self.database
                .update_filesystem_entry_inode(&existing_entry.id, inode)
                .await?;
            self.database
                .reset_media_probe_for_filesystem_entry(&existing_entry.id, size)
                .await?;
            if is_strm {
                self.database
                    .update_media_source_strm_target(
                        &existing_entry.id,
                        strm_target_kind,
                        external_url,
                    )
                    .await?;
            }
            return Ok(ScanReport {
                discovered_files: 1,
                created_items: ensured.created_items,
                changed_files: 1,
                ..ScanReport::default()
            });
        }

        let entry_id = FilesystemEntryId::new().to_string();
        self.database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: &entry_id,
                library_root_id: &root.id,
                relative_path: &relative_path,
                entry_kind: "FILE",
                size,
                modified_at,
                inode,
                fingerprint: &fingerprint,
                last_seen_generation: generation,
            })
            .await?;
        let source_id = SourceId::new().to_string();
        let container = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        self.database
            .insert_media_source(NewMediaSource {
                id: &source_id,
                item_id: &ensured.episode_id,
                source_kind: if is_strm { "STRM_URL" } else { "LOCAL_FILE" },
                filesystem_entry_id: &entry_id,
                edition_name: parsed.edition_name.as_deref(),
                quality_label: parsed.quality_label.as_deref(),
                container: &container,
                size,
                external_url,
                strm_target_kind,
                is_default: ensured.episode_created,
            })
            .await?;
        Ok(ScanReport {
            discovered_files: 1,
            created_items: ensured.created_items,
            created_sources: 1,
            ..ScanReport::default()
        })
    }

    async fn scan_unresolved_file(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        generation: &str,
    ) -> Result<ScanReport, ScannerError> {
        let is_strm = is_strm_file(path);
        let strm_target = if is_strm {
            Some(read_strm_target(path).await?)
        } else {
            None
        };
        let external_url = strm_target
            .as_ref()
            .and_then(|target| target.value.as_deref());
        let strm_target_kind = strm_target.as_ref().map(strm_target_kind_name);
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
        let fingerprint =
            compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
        if let Some(existing_entry) = self
            .database
            .find_filesystem_entry(&root.id, &relative_path)
            .await?
        {
            if existing_entry.fingerprint.as_deref() == Some(fingerprint.as_slice()) {
                let expected_parent_identity =
                    movie_parent_folder_identity(&root.id, &relative_path);
                if existing_entry.parent_identity_key.as_deref()
                    != expected_parent_identity.as_deref()
                    && let Some(item_id) = existing_entry.item_id.as_deref()
                {
                    self.database
                        .repair_movie_parent_folder(
                            library_id_text,
                            &root.id,
                            &relative_path,
                            item_id,
                        )
                        .await?;
                }
                if is_strm {
                    self.database
                        .update_media_source_strm_target(
                            &existing_entry.id,
                            strm_target_kind,
                            external_url,
                        )
                        .await?;
                }
                self.database
                    .mark_filesystem_entry_seen(&existing_entry.id, generation)
                    .await?;
                return Ok(ScanReport {
                    discovered_files: 1,
                    skipped_files: 1,
                    ..ScanReport::default()
                });
            }
            self.database
                .update_filesystem_entry(
                    &existing_entry.id,
                    size,
                    modified_at,
                    &fingerprint,
                    generation,
                )
                .await?;
            self.database
                .reset_media_probe_for_filesystem_entry(&existing_entry.id, size)
                .await?;
            if is_strm {
                self.database
                    .update_media_source_strm_target(
                        &existing_entry.id,
                        strm_target_kind,
                        external_url,
                    )
                    .await?;
            }
            return Ok(ScanReport {
                discovered_files: 1,
                changed_files: 1,
                ..ScanReport::default()
            });
        }
        let file_name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("Unresolved");
        let title = clean_hierarchy_title(file_name);
        let title = if title.is_empty() {
            "Unresolved".to_owned()
        } else {
            title
        };
        let sort_title = title.to_lowercase();
        let identity_key = format!("unresolved:{}:{}", root.id, relative_path);
        let item_id = ItemId::new().to_string();
        self.database
            .insert_hierarchy_item(NewHierarchyItem {
                id: &item_id,
                library_id: library_id_text,
                item_type: "UNRESOLVED",
                parent_id: None,
                series_id: None,
                season_number: None,
                episode_number: None,
                absolute_number: None,
                title: &title,
                sort_title: &sort_title,
                original_title: Some(&title),
                production_year: None,
                provider_ids_json: None,
                identification_status: "PENDING",
                identity_key: &identity_key,
            })
            .await?;
        let entry_id = FilesystemEntryId::new().to_string();
        self.database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: &entry_id,
                library_root_id: &root.id,
                relative_path: &relative_path,
                entry_kind: "FILE",
                size,
                modified_at,
                inode: inode.and_then(|value| i64::try_from(value).ok()),
                fingerprint: &fingerprint,
                last_seen_generation: generation,
            })
            .await?;
        let source_id = SourceId::new().to_string();
        let container = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        self.database
            .insert_media_source(NewMediaSource {
                id: &source_id,
                item_id: &item_id,
                source_kind: if is_strm { "STRM_URL" } else { "LOCAL_FILE" },
                filesystem_entry_id: &entry_id,
                edition_name: None,
                quality_label: None,
                container: &container,
                size,
                external_url,
                strm_target_kind,
                is_default: true,
            })
            .await?;
        self.database
            .repair_movie_parent_folder(library_id_text, &root.id, &relative_path, &item_id)
            .await?;
        Ok(ScanReport {
            discovered_files: 1,
            created_items: 1,
            created_sources: 1,
            ..ScanReport::default()
        })
    }

    async fn ensure_hierarchy_item(
        &self,
        item: NewHierarchyItem<'_>,
        legacy_identity: Option<&str>,
    ) -> Result<(String, bool), ScannerError> {
        if let Some(existing) = self
            .database
            .find_media_item_by_identity(item.identity_key)
            .await?
        {
            self.database
                .update_unconfirmed_hierarchy_item(
                    &existing.id,
                    item.title,
                    item.sort_title,
                    item.original_title,
                    item.production_year,
                    item.provider_ids_json,
                )
                .await?;
            self.database.restore_media_item(&existing.id).await?;
            return Ok((existing.id, false));
        }
        if let Some(legacy_identity) = legacy_identity
            && let Some(existing) = self
                .database
                .find_media_item_by_identity(legacy_identity)
                .await?
            && self
                .database
                .adopt_media_item_identity(&existing.id, item.identity_key)
                .await?
        {
            self.database
                .update_unconfirmed_hierarchy_item(
                    &existing.id,
                    item.title,
                    item.sort_title,
                    item.original_title,
                    item.production_year,
                    item.provider_ids_json,
                )
                .await?;
            self.database.restore_media_item(&existing.id).await?;
            return Ok((existing.id, false));
        }
        let id = item.id.to_owned();
        self.database.insert_hierarchy_item(item).await?;
        Ok((id, true))
    }

    fn episode_identity_key(
        root: &StoredLibraryRoot,
        hierarchy: &EpisodeHierarchy,
        parsed: &ParsedEpisodeFilename,
    ) -> String {
        Self::episode_identity_key_for_root(&root.id, hierarchy, parsed)
    }

    fn episode_identity_key_for_root(
        root_id: &str,
        hierarchy: &EpisodeHierarchy,
        parsed: &ParsedEpisodeFilename,
    ) -> String {
        let edition_key = parsed
            .edition_name
            .as_deref()
            .unwrap_or("standard")
            .to_ascii_lowercase();
        format!(
            "episode:{}:{}:season:{}:episode:{}:edition:{}",
            root_id, hierarchy.series_path, hierarchy.season_number, parsed.episode, edition_key
        )
    }

    async fn ensure_episode_hierarchy(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        relative_path: &str,
        parsed: &ParsedEpisodeFilename,
        hierarchy: &EpisodeHierarchy,
    ) -> Result<EnsuredEpisodeHierarchy, ScannerError> {
        let series_sort_title = hierarchy.series_title.to_lowercase();
        let series_identity = format!("series:{}:{}", root.id, hierarchy.series_path);
        let legacy_series_identity = legacy_series_identity(root, hierarchy);
        let series_provider_ids_json = provider_ids_json(&hierarchy.provider_ids);
        let series_new_id = ItemId::new().to_string();
        let (series_id, series_created) = self
            .ensure_hierarchy_item(
                NewHierarchyItem {
                    id: &series_new_id,
                    library_id: library_id_text,
                    item_type: "SERIES",
                    parent_id: None,
                    series_id: None,
                    season_number: None,
                    episode_number: None,
                    absolute_number: None,
                    title: &hierarchy.series_title,
                    sort_title: &series_sort_title,
                    original_title: Some(&hierarchy.series_title),
                    production_year: hierarchy.production_year.map(i64::from),
                    provider_ids_json: series_provider_ids_json.as_deref(),
                    identification_status: "PENDING",
                    identity_key: &series_identity,
                },
                legacy_series_identity.as_deref(),
            )
            .await?;
        let season_title = if hierarchy.season_number == 0 {
            "Specials".to_owned()
        } else {
            format!("Season {:02}", hierarchy.season_number)
        };
        let season_identity = format!("{series_identity}:season:{}", hierarchy.season_number);
        let season_sort_title = season_title.to_lowercase();
        let season_new_id = ItemId::new().to_string();
        let legacy_season_identity = legacy_series_identity
            .as_deref()
            .map(|identity| format!("{identity}:season:{}", hierarchy.season_number));
        let (season_id, season_created) = self
            .ensure_hierarchy_item(
                NewHierarchyItem {
                    id: &season_new_id,
                    library_id: library_id_text,
                    item_type: "SEASON",
                    parent_id: Some(&series_id),
                    series_id: Some(&series_id),
                    season_number: Some(i64::from(hierarchy.season_number)),
                    episode_number: None,
                    absolute_number: None,
                    title: &season_title,
                    sort_title: &season_sort_title,
                    original_title: Some(&season_title),
                    production_year: None,
                    provider_ids_json: None,
                    identification_status: "PENDING",
                    identity_key: &season_identity,
                },
                legacy_season_identity.as_deref(),
            )
            .await?;
        let episode_identity = Self::episode_identity_key(root, hierarchy, parsed);
        let episode_title = parsed.title.clone();
        let episode_sort_title = episode_title.to_lowercase();
        let episode_new_id = ItemId::new().to_string();
        let legacy_episode_identity = format!("episode:{}:{}", root.id, relative_path);
        let (episode_id, episode_created) = self
            .ensure_hierarchy_item(
                NewHierarchyItem {
                    id: &episode_new_id,
                    library_id: library_id_text,
                    item_type: "EPISODE",
                    parent_id: Some(&season_id),
                    series_id: Some(&series_id),
                    season_number: Some(i64::from(hierarchy.season_number)),
                    episode_number: Some(i64::from(parsed.episode)),
                    absolute_number: parsed.absolute_number.map(i64::from),
                    title: &episode_title,
                    sort_title: &episode_sort_title,
                    original_title: Some(&episode_title),
                    production_year: None,
                    provider_ids_json: None,
                    identification_status: "PENDING",
                    identity_key: &episode_identity,
                },
                Some(&legacy_episode_identity),
            )
            .await?;
        Ok(EnsuredEpisodeHierarchy {
            episode_id,
            created_items: usize::from(series_created)
                + usize::from(season_created)
                + usize::from(episode_created),
            episode_created,
        })
    }

    pub async fn scan_movie_directory(
        &self,
        library_id: LibraryId,
        directory: &Path,
    ) -> Result<ScanReport, ScannerError> {
        let library_id_text = library_id.to_string();
        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_none()
        {
            return Err(ScannerError::LibraryNotFound);
        }

        let canonical_directory =
            fs::canonicalize(directory)
                .await
                .map_err(|source| ScannerError::Io {
                    path: directory.to_owned(),
                    source,
                })?;
        let roots = self.database.list_library_roots(&library_id_text).await?;
        let root = roots
            .into_iter()
            .filter(|root| canonical_directory.starts_with(&root.canonical_path))
            .max_by_key(|root| root.canonical_path.len())
            .ok_or_else(|| {
                ScannerError::InvalidRelativePath(format!(
                    "directory is outside library roots: {}",
                    canonical_directory.display()
                ))
            })?;
        if !root.is_available {
            return Ok(ScanReport {
                unavailable_roots: 1,
                ..ScanReport::default()
            });
        }

        let generation = Uuid::now_v7().to_string();
        let mut report = ScanReport::default();
        let mut walker = FileBatchWalker::new(&canonical_directory);
        while let Some(files) = walker.next_batch(FILE_BATCH_SIZE).await? {
            report.merge(
                self.scan_movie_file_batch(
                    &library_id_text,
                    &root,
                    Path::new(&root.canonical_path),
                    &files,
                    &generation,
                )
                .await?,
            );
        }
        Ok(report)
    }

    async fn scan_movie_file_batch(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        files: &[PathBuf],
        generation: &str,
    ) -> Result<ScanReport, ScannerError> {
        if files.is_empty() {
            return Ok(ScanReport::default());
        }
        let relative_paths = files
            .iter()
            .map(|path| {
                path.strip_prefix(root_path)
                    .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                    .to_str()
                    .map(str::to_owned)
                    .ok_or(ScannerError::NonUtf8Path)
            })
            .collect::<Result<Vec<_>, ScannerError>>()?;
        let existing_entries = self
            .database
            .list_filesystem_entries_for_paths(&root.id, &relative_paths)
            .await?;
        let quick_results = self
            .scan_movie_files_if_unchanged(
                &root.id,
                root_path,
                files,
                &existing_entries,
                self.scan_concurrency,
            )
            .await?;
        let mut report = ScanReport::default();
        let mut seen_entry_ids = Vec::with_capacity(files.len());
        let mut new_paths = Vec::new();
        let mut changed_paths = Vec::new();
        for (path, quick_result) in files.iter().zip(quick_results) {
            if let Some((entry_id, quick_report)) = quick_result {
                seen_entry_ids.push(entry_id);
                report.merge(quick_report);
                continue;
            }
            let relative_path = path
                .strip_prefix(root_path)
                .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                .to_str()
                .ok_or(ScannerError::NonUtf8Path)?;
            if !existing_entries.contains_key(relative_path) {
                new_paths.push(path.clone());
                continue;
            }
            changed_paths.push(path.clone());
        }
        for changed_report in self
            .scan_movie_files_with_concurrency(
                library_id_text,
                root,
                root_path,
                &changed_paths,
                generation,
                self.scan_concurrency,
            )
            .await?
        {
            report.merge(changed_report);
        }

        let mut pending_new_files = Vec::with_capacity(new_paths.len());
        for file in self
            .prepare_new_movie_files(root_path, &new_paths)
            .await?
            .into_iter()
            .flatten()
        {
            pending_new_files.push(file);
            if pending_new_files.len() == FILE_BATCH_SIZE {
                self.flush_new_movie_files(
                    library_id_text,
                    root,
                    generation,
                    &mut pending_new_files,
                    &mut report,
                )
                .await?;
            }
        }
        self.flush_new_movie_files(
            library_id_text,
            root,
            generation,
            &mut pending_new_files,
            &mut report,
        )
        .await?;
        self.database
            .mark_filesystem_entries_seen_batch(&seen_entry_ids, generation)
            .await?;
        Ok(report)
    }

    async fn scan_movie_files_with_concurrency(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        paths: &[PathBuf],
        generation: &str,
        configured_concurrency: usize,
    ) -> Result<Vec<ScanReport>, ScannerError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let concurrency = configured_concurrency.max(1);
        // A regular movie scan can repair an old item's identity and reassign
        // one of its sources. Files with the same parsed movie identity must
        // therefore stay in input order; otherwise two source variants can
        // both observe the pre-repair rows and create duplicate logical items.
        // Different identities remain eligible for concurrent processing.
        let mut grouped_paths = Vec::<Vec<(usize, PathBuf)>>::new();
        let mut group_indexes = HashMap::<String, usize>::new();
        for (index, path) in paths.iter().cloned().enumerate() {
            let group_key = reconciliation_regular_group_key(
                &root.id,
                &path,
                MixedClassification::Movie,
                index,
            );
            let group_index = *group_indexes.entry(group_key).or_insert_with(|| {
                grouped_paths.push(Vec::new());
                grouped_paths.len() - 1
            });
            grouped_paths[group_index].push((index, path));
        }
        let mut tasks: JoinSet<ReconciliationRegularGroupTask> = JoinSet::new();
        let mut results = (0..paths.len()).map(|_| None).collect::<Vec<_>>();
        for group in grouped_paths {
            while tasks.len() >= concurrency {
                collect_reconciliation_regular_group_task(&mut tasks, &mut results).await?;
            }
            let scanner = self.clone();
            let library_id_text = library_id_text.to_owned();
            let root = root.clone();
            let root_path = root_path.to_owned();
            let generation = generation.to_owned();
            tasks.spawn(async move {
                let mut reports = Vec::with_capacity(group.len());
                for (index, path) in group {
                    let report = scanner
                        .scan_movie_file(&library_id_text, &root, &root_path, &path, &generation)
                        .await?;
                    reports.push((index, report));
                }
                Ok(reports)
            });
        }
        while !tasks.is_empty() {
            collect_reconciliation_regular_group_task(&mut tasks, &mut results).await?;
        }
        Ok(results.into_iter().flatten().collect())
    }

    async fn scan_movie_file_if_unchanged(
        &self,
        library_root_id: &str,
        root_path: &Path,
        path: &Path,
        existing_entry: &StoredFilesystemEntry,
    ) -> Result<Option<(String, ScanReport)>, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        if parse_movie_filename(file_name).is_none() || is_strm_file(path) {
            return Ok(None);
        }
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
        let fingerprint =
            compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
        if existing_entry.fingerprint.as_deref() != Some(fingerprint.as_slice()) {
            return Ok(None);
        }
        let expected_parent_identity =
            movie_parent_folder_identity(library_root_id, &relative_path);
        if existing_entry.parent_identity_key.as_deref() != expected_parent_identity.as_deref() {
            return Ok(None);
        }
        // CD-part and version-marker entries need the regular path to refresh their identity.
        if has_multi_part_marker(file_name) || has_source_variant_marker(file_name) {
            return Ok(None);
        }
        Ok(Some((
            existing_entry.id.clone(),
            ScanReport {
                discovered_files: 1,
                skipped_files: 1,
                ..ScanReport::default()
            },
        )))
    }

    async fn scan_movie_files_if_unchanged(
        &self,
        library_root_id: &str,
        root_path: &Path,
        paths: &[PathBuf],
        existing_entries: &HashMap<String, StoredFilesystemEntry>,
        configured_concurrency: usize,
    ) -> Result<Vec<Option<(String, ScanReport)>>, ScannerError> {
        let mut results = (0..paths.len()).map(|_| None).collect::<Vec<_>>();
        let mut tasks: JoinSet<MovieFingerprintTask> = JoinSet::new();
        let concurrency = configured_concurrency.clamp(1, FINGERPRINT_CHECK_CONCURRENCY);
        for (index, path) in paths.iter().enumerate() {
            let relative_path = path
                .strip_prefix(root_path)
                .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                .to_str()
                .ok_or(ScannerError::NonUtf8Path)?;
            let Some(existing_entry) = existing_entries.get(relative_path).cloned() else {
                continue;
            };
            while tasks.len() >= concurrency {
                collect_movie_fingerprint_task(&mut tasks, &mut results).await?;
            }
            let scanner = self.clone();
            let root_path = root_path.to_owned();
            let path = path.clone();
            let library_root_id = library_root_id.to_owned();
            tasks.spawn(async move {
                let result = scanner
                    .scan_movie_file_if_unchanged(
                        &library_root_id,
                        &root_path,
                        &path,
                        &existing_entry,
                    )
                    .await?;
                Ok((index, result))
            });
        }
        while !tasks.is_empty() {
            collect_movie_fingerprint_task(&mut tasks, &mut results).await?;
        }
        Ok(results)
    }

    async fn scan_file_if_fingerprint_unchanged(
        &self,
        root_path: &Path,
        path: &Path,
        existing_entry: &StoredFilesystemEntry,
    ) -> Result<Option<(String, ScanReport)>, ScannerError> {
        let (_, fingerprint) = current_file_fingerprint(root_path, path).await?;
        if existing_entry.fingerprint.as_deref() != Some(fingerprint.as_slice()) {
            return Ok(None);
        }
        Ok(Some((
            existing_entry.id.clone(),
            ScanReport {
                discovered_files: 1,
                skipped_files: 1,
                ..ScanReport::default()
            },
        )))
    }

    async fn scan_episode_file_if_unchanged(
        &self,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        existing_entries: &HashMap<String, StoredFilesystemEntry>,
    ) -> Result<Option<(String, ScanReport)>, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        if parse_episode_filename(file_name).is_none() {
            return Ok(None);
        }
        if is_strm_file(path) {
            return Ok(None);
        }
        let (relative_path, fingerprint) = current_file_fingerprint(root_path, path).await?;
        let Some(existing_entry) = existing_entries.get(&relative_path) else {
            return Ok(None);
        };
        let Some((entry_id, report, provider_update)) = self
            .scan_episode_file_if_unchanged_entry(
                root,
                path,
                &relative_path,
                &fingerprint,
                existing_entry,
            )
            .await?
        else {
            return Ok(None);
        };
        if let Some((series_identity, provider_ids_json)) = provider_update {
            self.database
                .update_local_provider_ids_for_identity_if_empty(
                    &series_identity,
                    &provider_ids_json,
                )
                .await?;
        }
        Ok(Some((entry_id, report)))
    }

    async fn scan_episode_file_if_unchanged_entry(
        &self,
        root: &StoredLibraryRoot,
        path: &Path,
        relative_path: &str,
        fingerprint: &[u8],
        existing_entry: &StoredFilesystemEntry,
    ) -> Result<EpisodeQuickResult, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let Some(parsed) = parse_episode_filename(file_name) else {
            return Ok(None);
        };
        if is_strm_file(path) {
            return Ok(None);
        }
        if existing_entry.fingerprint.as_deref() != Some(fingerprint) {
            return Ok(None);
        }
        if existing_entry.item_id.is_none() {
            return Ok(None);
        }
        let hierarchy = episode_hierarchy(relative_path, &parsed);
        let series_identity = format!("series:{}:{}", root.id, hierarchy.series_path);
        let episode_identity = Self::episode_identity_key(root, &hierarchy, &parsed);
        if existing_entry.item_identity_key.as_deref() != Some(episode_identity.as_str()) {
            return Ok(None);
        }
        let series_provider_ids_missing = existing_entry
            .series_provider_ids_json
            .as_deref()
            .is_none_or(|value| value.is_empty() || value == "{}");
        let provider_update = if !hierarchy.provider_ids.is_empty() && series_provider_ids_missing {
            provider_ids_json(&hierarchy.provider_ids).map(|json| (series_identity, json))
        } else {
            None
        };
        Ok(Some((
            existing_entry.id.clone(),
            ScanReport {
                discovered_files: 1,
                skipped_files: 1,
                ..ScanReport::default()
            },
            provider_update,
        )))
    }

    async fn scan_episode_files_if_unchanged(
        &self,
        root: &StoredLibraryRoot,
        root_path: &Path,
        paths: &[PathBuf],
        existing_entries: &HashMap<String, StoredFilesystemEntry>,
        configured_concurrency: usize,
    ) -> Result<Vec<EpisodeQuickResult>, ScannerError> {
        let mut results = (0..paths.len()).map(|_| None).collect::<Vec<_>>();
        let mut tasks: JoinSet<EpisodeFingerprintTask> = JoinSet::new();
        let concurrency = configured_concurrency.clamp(1, FINGERPRINT_CHECK_CONCURRENCY);
        for (index, path) in paths.iter().enumerate() {
            let relative_path = path
                .strip_prefix(root_path)
                .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                .to_str()
                .ok_or(ScannerError::NonUtf8Path)?;
            let Some(existing_entry) = existing_entries.get(relative_path).cloned() else {
                continue;
            };
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| parse_episode_filename(name).is_none() || is_strm_file(path))
            {
                continue;
            }
            while tasks.len() >= concurrency {
                collect_episode_fingerprint_task(&mut tasks, &mut results).await?;
            }
            let scanner = self.clone();
            let root = root.clone();
            let root_path = root_path.to_owned();
            let path = path.clone();
            let relative_path = relative_path.to_owned();
            tasks.spawn(async move {
                let (_, fingerprint) = current_file_fingerprint(&root_path, &path).await?;
                let result = scanner
                    .scan_episode_file_if_unchanged_entry(
                        &root,
                        &path,
                        &relative_path,
                        &fingerprint,
                        &existing_entry,
                    )
                    .await?;
                Ok((index, result))
            });
        }
        while !tasks.is_empty() {
            collect_episode_fingerprint_task(&mut tasks, &mut results).await?;
        }
        Ok(results)
    }

    async fn file_has_moved_entry(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
    ) -> Result<bool, ScannerError> {
        let metadata = fs::metadata(path)
            .await
            .map_err(|source| ScannerError::Io {
                path: path.to_owned(),
                source,
            })?;
        let Some(inode) = file_identity(&metadata)
            .1
            .and_then(|value| i64::try_from(value).ok())
        else {
            return Ok(false);
        };
        let relative_path = path
            .strip_prefix(root_path)
            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
            .to_str()
            .ok_or(ScannerError::NonUtf8Path)?;
        Ok(self
            .database
            .find_filesystem_entry_by_inode(library_id_text, &root.id, inode, relative_path)
            .await?
            .is_some())
    }

    async fn episode_path_has_legacy_identity(
        &self,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
    ) -> Result<bool, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(false);
        };
        let Some(parsed) = parse_episode_filename(file_name) else {
            return Ok(false);
        };
        let relative_path = path
            .strip_prefix(root_path)
            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
            .to_str()
            .ok_or(ScannerError::NonUtf8Path)?;
        let hierarchy = episode_hierarchy(relative_path, &parsed);
        let legacy_series = legacy_series_identity(root, &hierarchy);
        let legacy_season = legacy_series
            .as_deref()
            .map(|identity| format!("{identity}:season:{}", hierarchy.season_number));
        let legacy_episode = format!("episode:{}:{relative_path}", root.id);
        for identity in [legacy_series, legacy_season, Some(legacy_episode)] {
            if let Some(identity) = identity
                && self
                    .database
                    .find_media_item_by_identity(&identity)
                    .await?
                    .is_some()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn scan_unresolved_file_if_unchanged(
        &self,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        existing_entries: &HashMap<String, StoredFilesystemEntry>,
    ) -> Result<Option<(String, ScanReport)>, ScannerError> {
        if is_strm_file(path) {
            return Ok(None);
        }
        let (relative_path, fingerprint) = current_file_fingerprint(root_path, path).await?;
        let Some(existing_entry) = existing_entries.get(&relative_path) else {
            return Ok(None);
        };
        if existing_entry.fingerprint.as_deref() != Some(fingerprint.as_slice()) {
            return Ok(None);
        }
        let expected_parent_identity = movie_parent_folder_identity(&root.id, &relative_path);
        if existing_entry.item_type.as_deref() == Some("MOVIE")
            && existing_entry.parent_identity_key.as_deref() != expected_parent_identity.as_deref()
        {
            return Ok(None);
        }
        Ok(Some((
            existing_entry.id.clone(),
            ScanReport {
                discovered_files: 1,
                skipped_files: 1,
                ..ScanReport::default()
            },
        )))
    }

    async fn scan_sidecar_file(
        &self,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        existing_entries: &HashMap<String, StoredFilesystemEntry>,
        generation: &str,
    ) -> Result<(String, bool), ScannerError> {
        let (relative_path, fingerprint) = current_file_fingerprint(root_path, path).await?;
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
        let (_, inode) = file_identity(&metadata);
        let inode = inode.and_then(|value| i64::try_from(value).ok());
        if let Some(existing_entry) = existing_entries.get(&relative_path) {
            if existing_entry.fingerprint.as_deref() == Some(fingerprint.as_slice()) {
                return Ok((existing_entry.id.clone(), false));
            }
            self.database
                .update_filesystem_entry(
                    &existing_entry.id,
                    size,
                    modified_at,
                    &fingerprint,
                    generation,
                )
                .await?;
            self.database
                .update_filesystem_entry_inode(&existing_entry.id, inode)
                .await?;
            return Ok((existing_entry.id.clone(), true));
        }
        let entry_id = FilesystemEntryId::new().to_string();
        self.database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: &entry_id,
                library_root_id: &root.id,
                relative_path: &relative_path,
                entry_kind: "FILE",
                size,
                modified_at,
                inode,
                fingerprint: &fingerprint,
                last_seen_generation: generation,
            })
            .await?;
        Ok((entry_id, true))
    }

    async fn prepare_new_movie_file(
        &self,
        root_path: &Path,
        path: &Path,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        self.prepare_new_movie_file_with_folder_provider_ids(root_path, path, None)
            .await
    }

    async fn prepare_new_movie_file_with_folder_provider_ids(
        &self,
        root_path: &Path,
        path: &Path,
        folder_provider_ids: Option<&BTreeMap<String, String>>,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        self.prepare_new_movie_file_with_strm_target(root_path, path, folder_provider_ids, None)
            .await
    }

    async fn prepare_manifest_movie_file(
        &self,
        path: &Path,
        observation: &NewScanManifestEntry,
        strm_target: Option<StrmTarget>,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        self.prepare_new_movie_file_with_manifest_observation(path, observation, strm_target)
            .await
    }

    async fn prepare_new_movie_file_with_manifest_observation(
        &self,
        path: &Path,
        observation: &NewScanManifestEntry,
        manifest_strm_target: Option<StrmTarget>,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let Some(parsed_name) = parse_movie_filename(file_name) else {
            return Ok(None);
        };
        let provider_ids = movie_provider_ids(path, &parsed_name.provider_ids);
        let provider_ids_json = provider_ids_json(&provider_ids);
        let is_strm = is_strm_file(path);
        let strm_target = if is_strm {
            Some(match manifest_strm_target {
                Some(target) => target,
                None => read_strm_target(path).await?,
            })
        } else {
            None
        };
        let external_url = strm_target
            .as_ref()
            .and_then(|target| target.value.as_deref());
        let strm_target_kind = strm_target.as_ref().map(strm_target_kind_name);
        let container = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        Ok(Some(NewMovieFile {
            filesystem_entry_id: FilesystemEntryId::new().to_string(),
            source_id: SourceId::new().to_string(),
            relative_path: observation.relative_path.clone(),
            size: observation.size,
            modified_at: observation.modified_at,
            fingerprint: observation.fingerprint.clone(),
            title: parsed_name.title.clone(),
            sort_title: parsed_name.sort_title,
            original_title: parsed_name.title,
            production_year: parsed_name.production_year.map(i64::from),
            provider_ids_json,
            source_kind: if is_strm {
                "STRM_URL".to_owned()
            } else {
                "LOCAL_FILE".to_owned()
            },
            strm_target_kind: strm_target_kind.map(str::to_owned),
            edition_name: parsed_name.edition_name,
            quality_label: parsed_name.quality_label,
            container,
            external_url: external_url.map(str::to_owned),
        }))
    }

    async fn prepare_new_movie_file_with_strm_target(
        &self,
        root_path: &Path,
        path: &Path,
        folder_provider_ids: Option<&BTreeMap<String, String>>,
        manifest_strm_target: Option<StrmTarget>,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let Some(parsed_name) = parse_movie_filename(file_name) else {
            return Ok(None);
        };
        let provider_ids = match folder_provider_ids {
            Some(folder_provider_ids) => {
                let mut provider_ids = parsed_name.provider_ids.clone();
                for (provider, provider_id) in folder_provider_ids {
                    provider_ids
                        .entry(provider.clone())
                        .or_insert_with(|| provider_id.clone());
                }
                provider_ids
            }
            None => movie_provider_ids(path, &parsed_name.provider_ids),
        };
        let provider_ids_json = provider_ids_json(&provider_ids);
        let is_strm = is_strm_file(path);
        let strm_target = if is_strm {
            Some(match manifest_strm_target {
                Some(target) => target,
                None => read_strm_target(path).await?,
            })
        } else {
            None
        };
        let external_url = strm_target
            .as_ref()
            .and_then(|target| target.value.as_deref());
        let strm_target_kind = strm_target.as_ref().map(strm_target_kind_name);
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
        let fingerprint =
            compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
        let container = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        Ok(Some(NewMovieFile {
            filesystem_entry_id: FilesystemEntryId::new().to_string(),
            source_id: SourceId::new().to_string(),
            relative_path,
            size,
            modified_at,
            fingerprint,
            title: parsed_name.title.clone(),
            sort_title: parsed_name.sort_title,
            original_title: parsed_name.title,
            production_year: parsed_name.production_year.map(i64::from),
            provider_ids_json,
            source_kind: if is_strm {
                "STRM_URL".to_owned()
            } else {
                "LOCAL_FILE".to_owned()
            },
            strm_target_kind: strm_target_kind.map(str::to_owned),
            edition_name: parsed_name.edition_name,
            quality_label: parsed_name.quality_label,
            container,
            external_url: external_url.map(str::to_owned),
        }))
    }

    async fn prepare_new_episode_file(
        &self,
        root_id: &str,
        root_path: &Path,
        path: &Path,
    ) -> Result<Option<NewEpisodeFile>, ScannerError> {
        self.prepare_new_episode_file_with_strm_target(root_id, root_path, path, None)
            .await
    }

    async fn prepare_manifest_episode_file(
        &self,
        root_id: &str,
        path: &Path,
        observation: &NewScanManifestEntry,
        strm_target: Option<StrmTarget>,
    ) -> Result<Option<NewEpisodeFile>, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let Some(parsed) = parse_episode_filename(file_name) else {
            return Ok(None);
        };
        let is_strm = is_strm_file(path);
        let strm_target = if is_strm {
            Some(match strm_target {
                Some(target) => target,
                None => read_strm_target(path).await?,
            })
        } else {
            None
        };
        let external_url = strm_target
            .as_ref()
            .and_then(|target| target.value.as_deref());
        let strm_target_kind = strm_target.as_ref().map(strm_target_kind_name);
        let hierarchy = episode_hierarchy(&observation.relative_path, &parsed);
        let series_identity = format!("series:{root_id}:{}", hierarchy.series_path);
        let season_identity = format!("{series_identity}:season:{}", hierarchy.season_number);
        let episode_identity = Self::episode_identity_key_for_root(root_id, &hierarchy, &parsed);
        let series_provider_ids_json = provider_ids_json(&hierarchy.provider_ids);
        let container = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let series_title = hierarchy.series_title;
        let series_sort_title = series_title.to_lowercase();
        Ok(Some(NewEpisodeFile {
            filesystem_entry_id: FilesystemEntryId::new().to_string(),
            source_id: SourceId::new().to_string(),
            relative_path: observation.relative_path.clone(),
            size: observation.size,
            modified_at: observation.modified_at,
            inode: observation.inode,
            fingerprint: observation.fingerprint.clone(),
            series_identity,
            series_title,
            series_sort_title,
            series_production_year: hierarchy.production_year.map(i64::from),
            series_provider_ids_json,
            season_identity,
            season_number: i64::from(hierarchy.season_number),
            episode_identity,
            episode_title: parsed.title.clone(),
            episode_sort_title: parsed.title.to_lowercase(),
            episode_number: i64::from(parsed.episode),
            episode_absolute_number: parsed.absolute_number.map(i64::from),
            source_kind: if is_strm {
                "STRM_URL".to_owned()
            } else {
                "LOCAL_FILE".to_owned()
            },
            strm_target_kind: strm_target_kind.map(str::to_owned),
            edition_name: parsed.edition_name,
            quality_label: parsed.quality_label,
            container,
            external_url: external_url.map(str::to_owned),
        }))
    }

    async fn prepare_new_episode_file_with_strm_target(
        &self,
        root_id: &str,
        root_path: &Path,
        path: &Path,
        manifest_strm_target: Option<StrmTarget>,
    ) -> Result<Option<NewEpisodeFile>, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let Some(parsed) = parse_episode_filename(file_name) else {
            return Ok(None);
        };
        let is_strm = is_strm_file(path);
        let strm_target = if is_strm {
            Some(match manifest_strm_target {
                Some(target) => target,
                None => read_strm_target(path).await?,
            })
        } else {
            None
        };
        let external_url = strm_target
            .as_ref()
            .and_then(|target| target.value.as_deref());
        let strm_target_kind = strm_target.as_ref().map(strm_target_kind_name);
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
        let fingerprint =
            compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
        let hierarchy = episode_hierarchy(&relative_path, &parsed);
        let series_identity = format!("series:{root_id}:{}", hierarchy.series_path);
        let season_identity = format!("{series_identity}:season:{}", hierarchy.season_number);
        let episode_identity = Self::episode_identity_key_for_root(root_id, &hierarchy, &parsed);
        let series_provider_ids_json = provider_ids_json(&hierarchy.provider_ids);
        let container = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let series_title = hierarchy.series_title;
        let series_sort_title = series_title.to_lowercase();
        Ok(Some(NewEpisodeFile {
            filesystem_entry_id: FilesystemEntryId::new().to_string(),
            source_id: SourceId::new().to_string(),
            relative_path,
            size,
            modified_at,
            inode: inode.and_then(|value| i64::try_from(value).ok()),
            fingerprint,
            series_identity,
            series_title,
            series_sort_title,
            series_production_year: hierarchy.production_year.map(i64::from),
            series_provider_ids_json,
            season_identity,
            season_number: i64::from(hierarchy.season_number),
            episode_identity,
            episode_title: parsed.title.clone(),
            episode_sort_title: parsed.title.to_lowercase(),
            episode_number: i64::from(parsed.episode),
            episode_absolute_number: parsed.absolute_number.map(i64::from),
            source_kind: if is_strm {
                "STRM_URL".to_owned()
            } else {
                "LOCAL_FILE".to_owned()
            },
            strm_target_kind: strm_target_kind.map(str::to_owned),
            edition_name: parsed.edition_name,
            quality_label: parsed.quality_label,
            container,
            external_url: external_url.map(str::to_owned),
        }))
    }

    async fn prepare_manifest_unresolved_file(
        &self,
        root: &StoredLibraryRoot,
        path: &Path,
        observation: &NewScanManifestEntry,
        manifest_strm_target: Option<StrmTarget>,
    ) -> Result<NewScanManifestUnresolvedFile, ScannerError> {
        let relative_path = observation.relative_path.clone();
        let is_strm = is_strm_file(path);
        let strm_target = if is_strm {
            Some(match manifest_strm_target {
                Some(target) => target,
                None => read_strm_target(path).await?,
            })
        } else {
            None
        };
        let file_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("Unresolved");
        let cleaned_title = clean_hierarchy_title(file_stem);
        let title = if cleaned_title.is_empty() {
            "Unresolved".to_owned()
        } else {
            cleaned_title
        };
        let container = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        Ok(NewScanManifestUnresolvedFile {
            item_id: ItemId::new().to_string(),
            filesystem_entry_id: FilesystemEntryId::new().to_string(),
            source_id: SourceId::new().to_string(),
            identity_key: format!("unresolved:{}:{relative_path}", root.id),
            relative_path,
            size: observation.size,
            modified_at: observation.modified_at,
            inode: observation.inode,
            fingerprint: observation.fingerprint.clone(),
            title,
            source_kind: if is_strm {
                "STRM_URL".to_owned()
            } else {
                "LOCAL_FILE".to_owned()
            },
            container,
            external_url: strm_target.as_ref().and_then(|target| target.value.clone()),
            strm_target_kind: strm_target
                .as_ref()
                .map(strm_target_kind_name)
                .map(str::to_owned),
        })
    }

    async fn prepare_new_episode_files(
        &self,
        root: &StoredLibraryRoot,
        root_path: &Path,
        paths: &[PathBuf],
    ) -> Result<Vec<Option<NewEpisodeFile>>, ScannerError> {
        self.prepare_new_episode_files_with_concurrency(
            root,
            root_path,
            paths,
            self.scan_concurrency,
        )
        .await
    }

    async fn prepare_new_episode_files_with_concurrency(
        &self,
        root: &StoredLibraryRoot,
        root_path: &Path,
        paths: &[PathBuf],
        configured_concurrency: usize,
    ) -> Result<Vec<Option<NewEpisodeFile>>, ScannerError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let concurrency = configured_concurrency.max(1);
        let mut tasks: JoinSet<Result<(usize, Option<NewEpisodeFile>), ScannerError>> =
            JoinSet::new();
        let mut results = (0..paths.len()).map(|_| None).collect::<Vec<_>>();
        for (index, path) in paths.iter().cloned().enumerate() {
            if tasks.len() >= concurrency {
                collect_episode_preparation_task(&mut tasks, &mut results).await?;
            }
            let scanner = self.clone();
            let root_id = root.id.clone();
            let root_path = root_path.to_owned();
            tasks.spawn(async move {
                let prepared = scanner
                    .prepare_new_episode_file(&root_id, &root_path, &path)
                    .await?;
                Ok((index, prepared))
            });
        }
        while !tasks.is_empty() {
            collect_episode_preparation_task(&mut tasks, &mut results).await?;
        }
        Ok(results)
    }

    async fn prepare_new_movie_files(
        &self,
        root_path: &Path,
        paths: &[PathBuf],
    ) -> Result<Vec<Option<NewMovieFile>>, ScannerError> {
        self.prepare_new_movie_files_with_concurrency(root_path, paths, self.scan_concurrency)
            .await
    }

    async fn prepare_new_movie_files_with_concurrency(
        &self,
        root_path: &Path,
        paths: &[PathBuf],
        configured_concurrency: usize,
    ) -> Result<Vec<Option<NewMovieFile>>, ScannerError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let concurrency = configured_concurrency.max(1);
        let mut folder_provider_ids = HashMap::<PathBuf, BTreeMap<String, String>>::new();
        for path in paths {
            let directory = path.parent().unwrap_or(root_path).to_owned();
            folder_provider_ids
                .entry(directory)
                .or_insert_with(|| movie_folder_provider_ids(path));
        }

        let mut tasks: JoinSet<Result<(usize, Option<NewMovieFile>), ScannerError>> =
            JoinSet::new();
        let mut results = Vec::with_capacity(paths.len());
        for (index, path) in paths.iter().cloned().enumerate() {
            if tasks.len() >= concurrency {
                collect_movie_preparation_task(&mut tasks, &mut results).await?;
            }
            let scanner = self.clone();
            let root_path = root_path.to_owned();
            let folder_provider_ids = folder_provider_ids
                .get(path.parent().unwrap_or(root_path.as_path()))
                .cloned()
                .unwrap_or_default();
            tasks.spawn(async move {
                let prepared = scanner
                    .prepare_new_movie_file_with_folder_provider_ids(
                        &root_path,
                        &path,
                        Some(&folder_provider_ids),
                    )
                    .await?;
                Ok((index, prepared))
            });
        }
        while !tasks.is_empty() {
            collect_movie_preparation_task(&mut tasks, &mut results).await?;
        }
        results.sort_unstable_by_key(|(index, _)| *index);
        Ok(results.into_iter().map(|(_, prepared)| prepared).collect())
    }

    async fn flush_new_movie_files(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        generation: &str,
        files: &mut Vec<NewMovieFile>,
        report: &mut ScanReport,
    ) -> Result<(), ScannerError> {
        if files.is_empty() {
            return Ok(());
        }
        let file_count = files.len();
        report.created_items += self
            .database
            .insert_movie_files_batch(library_id_text, &root.id, generation, files)
            .await?;
        report.discovered_files += file_count;
        report.created_sources += file_count;
        files.clear();
        Ok(())
    }

    pub(crate) async fn scan_movie_file(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        generation: &str,
    ) -> Result<ScanReport, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(ScanReport::default());
        };
        let Some(parsed_name) = parse_movie_filename(file_name) else {
            return Ok(ScanReport::default());
        };
        let provider_ids = movie_provider_ids(path, &parsed_name.provider_ids);
        let provider_ids_json = provider_ids_json(&provider_ids);
        let is_strm = is_strm_file(path);
        let strm_target = if is_strm {
            Some(read_strm_target(path).await?)
        } else {
            None
        };
        let external_url = strm_target
            .as_ref()
            .and_then(|target| target.value.as_deref());
        let strm_target_kind = strm_target.as_ref().map(strm_target_kind_name);
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
        let fingerprint =
            compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
        let mut report = ScanReport {
            discovered_files: 1,
            ..ScanReport::default()
        };
        let existing_entry = self
            .database
            .find_filesystem_entry(&root.id, &relative_path)
            .await?;
        let expected_parent_identity = movie_parent_folder_identity(&root.id, &relative_path);
        if let Some(existing_entry) = existing_entry.as_ref()
            && existing_entry.item_type.as_deref() == Some("MOVIE")
            && existing_entry.parent_identity_key.as_deref() != expected_parent_identity.as_deref()
            && let Some(item_id) = existing_entry.item_id.as_deref()
        {
            self.database
                .repair_movie_parent_folder(library_id_text, &root.id, &relative_path, item_id)
                .await?;
        }
        if !has_multi_part_marker(file_name)
            && !has_source_variant_marker(file_name)
            && let Some(existing_entry) = existing_entry.as_ref()
        {
            if existing_entry.fingerprint.as_deref() == Some(fingerprint.as_slice()) {
                if is_strm {
                    self.database
                        .update_media_source_strm_target(
                            &existing_entry.id,
                            strm_target_kind,
                            external_url,
                        )
                        .await?;
                }
                self.database
                    .mark_filesystem_entry_seen(&existing_entry.id, generation)
                    .await?;
                report.skipped_files = 1;
                return Ok(report);
            }
            self.database
                .update_filesystem_entry(
                    &existing_entry.id,
                    size,
                    modified_at,
                    &fingerprint,
                    generation,
                )
                .await?;
            self.database
                .reset_media_probe_for_filesystem_entry(&existing_entry.id, size)
                .await?;
            if is_strm {
                self.database
                    .update_media_source_strm_target(
                        &existing_entry.id,
                        strm_target_kind,
                        external_url,
                    )
                    .await?;
            }
            report.changed_files = 1;
            return Ok(report);
        }

        let existing_item = self
            .database
            .find_media_item(
                library_id_text,
                &parsed_name.sort_title,
                parsed_name.production_year.map(i64::from),
            )
            .await?;
        let (item_id, created_item) = if let Some(item) = existing_item {
            self.database.restore_media_item(&item.id).await?;
            (item.id, false)
        } else {
            let item_id = ItemId::new().to_string();
            self.database
                .insert_media_item(NewMediaItem {
                    id: &item_id,
                    library_id: library_id_text,
                    title: &parsed_name.title,
                    sort_title: &parsed_name.sort_title,
                    original_title: Some(&parsed_name.title),
                    production_year: parsed_name.production_year.map(i64::from),
                    provider_ids_json: provider_ids_json.as_deref(),
                })
                .await?;
            (item_id, true)
        };
        if let Some(provider_ids_json) = provider_ids_json.as_deref() {
            self.database
                .update_local_provider_ids_if_empty(&item_id, provider_ids_json)
                .await?;
        }
        self.database
            .repair_movie_parent_folder(library_id_text, &root.id, &relative_path, &item_id)
            .await?;
        if let Some(existing_entry) = existing_entry {
            let reassigned = self
                .database
                .reassign_media_source_item(&existing_entry.id, &item_id)
                .await?;
            self.database
                .update_media_source_variant_labels(
                    &existing_entry.id,
                    parsed_name.edition_name.as_deref(),
                    parsed_name.quality_label.as_deref(),
                )
                .await?;
            if existing_entry.fingerprint.as_deref() == Some(fingerprint.as_slice()) {
                if is_strm {
                    self.database
                        .update_media_source_strm_target(
                            &existing_entry.id,
                            strm_target_kind,
                            external_url,
                        )
                        .await?;
                }
                self.database
                    .mark_filesystem_entry_seen(&existing_entry.id, generation)
                    .await?;
                report.created_items = usize::from(created_item);
                report.changed_files = usize::from(reassigned);
                report.skipped_files = 1;
                return Ok(report);
            }
            self.database
                .update_filesystem_entry(
                    &existing_entry.id,
                    size,
                    modified_at,
                    &fingerprint,
                    generation,
                )
                .await?;
            self.database
                .reset_media_probe_for_filesystem_entry(&existing_entry.id, size)
                .await?;
            if is_strm {
                self.database
                    .update_media_source_strm_target(
                        &existing_entry.id,
                        strm_target_kind,
                        external_url,
                    )
                    .await?;
            }
            report.created_items = usize::from(created_item);
            report.changed_files = 1;
            return Ok(report);
        }

        let entry_id = FilesystemEntryId::new().to_string();
        self.database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: &entry_id,
                library_root_id: &root.id,
                relative_path: &relative_path,
                entry_kind: "FILE",
                size,
                modified_at,
                inode: inode.and_then(|value| i64::try_from(value).ok()),
                fingerprint: &fingerprint,
                last_seen_generation: generation,
            })
            .await?;
        let source_id = SourceId::new().to_string();
        let container = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        self.database
            .insert_media_source(NewMediaSource {
                id: &source_id,
                item_id: &item_id,
                source_kind: if is_strm { "STRM_URL" } else { "LOCAL_FILE" },
                filesystem_entry_id: &entry_id,
                edition_name: parsed_name.edition_name.as_deref(),
                quality_label: parsed_name.quality_label.as_deref(),
                container: &container,
                size,
                external_url,
                strm_target_kind,
                is_default: created_item,
            })
            .await?;
        report.created_sources = 1;
        report.created_items = if created_item { 1 } else { 0 };
        Ok(report)
    }
}

#[derive(Clone)]
pub struct ScanJobService {
    scanner: LibraryScanner,
    database: Database,
    admin_events: AdminEventHub,
    user_events: UserEventHub,
    scan_lock: Arc<Semaphore>,
    library_covers: Option<LibraryCoverService>,
    strm_probe: Option<StrmProbeService>,
    people: Option<PeopleService>,
    local_nfo: Option<LocalNfoMetadataStore>,
    home: Option<HomeService>,
    webhooks: Option<WebhookService>,
    resources: ResourceMetrics,
    default_scan_concurrency: usize,
    scan_concurrency_override: Option<usize>,
    cancellation_flags: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    metadata_notifications: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    lite_manifest_discovery: Arc<Mutex<HashMap<String, LiteManifestDiscoverySession>>>,
}

struct LocalMetadataWorkerHandle {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    job_id: String,
    notifications: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncrementalScanChange {
    pub root_id: String,
    pub relative_path: String,
    pub kind: ChangeKind,
}

impl ScanJobService {
    pub fn new(database: Database) -> Self {
        Self {
            scanner: LibraryScanner::new(database.clone()),
            database,
            admin_events: AdminEventHub::new(),
            user_events: UserEventHub::new(),
            scan_lock: Arc::new(Semaphore::new(1)),
            library_covers: None,
            strm_probe: None,
            people: None,
            local_nfo: None,
            home: None,
            webhooks: None,
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
            metadata_notifications: Arc::new(Mutex::new(HashMap::new())),
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

        let root_ids = self
            .database
            .list_scan_manifest_root_ids(manifest_id)
            .await?;
        let mut session = LiteManifestDiscoverySession::default();
        for root_id in root_ids {
            let state = self
                .database
                .get_scan_manifest_root_identity(manifest_id, &root_id)
                .await?
                .map(|(state, _, _)| state);
            if matches!(state.as_deref(), Some("COMPLETE" | "UNAVAILABLE")) {
                continue;
            }
            session.directories.push_back((root_id, String::new()));
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
    ) -> Vec<(String, String)> {
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
                .map(|directory| (root_id.to_owned(), directory)),
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
                .retain(|(queued_root_id, _)| queued_root_id != root_id);
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
                self.user_events.publish_home_now().await;
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
    ) -> Result<(), ScanJobError> {
        let library_id = library_id.to_string();
        for status in ["PENDING", "RUNNING"] {
            let jobs = self
                .database
                .list_scan_jobs(Some(status), 0, 10_000)
                .await?;
            for job in jobs.into_iter().filter(|job| job.library_id == library_id) {
                self.cancellation_flag(&job.id)
                    .store(true, Ordering::Release);
                self.database.request_scan_job_cancel(&job.id).await?;
            }
        }
        Ok(())
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
        self.admin_events.publish(AdminEventScope::Jobs);
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
            .get_or_create_incremental_scan_job_reusing_active(&library_id_text, true)
            .await?;
        self.cancellation_flag(&job.id);
        for (root_id, relative_path, kind) in valid_changes {
            self.database
                .enqueue_incremental_scan_path(
                    &job.id,
                    &root_id,
                    &relative_path,
                    change_kind_name(kind),
                )
                .await?;
        }
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
        for root in roots {
            self.database
                .enqueue_incremental_scan_path(&job.id, &root.id, ".", "MODIFY")
                .await?;
        }
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
        match self
            .run_batch_unlocked(job_id, batch_size, stream_files_during_discovery)
            .await
        {
            Ok(report) => Ok(report),
            Err(error) => {
                self.fail_unhandled_scan_job(job_id, &error).await?;
                Err(error)
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
                        manifest.workflow_version == 2,
                        lite_mode,
                    )
                    .await
                }
                "READY_TO_DIFF" => {
                    self.run_scan_manifest_diff_batch(
                        &job,
                        &manifest.id,
                        &cancellation,
                        manifest.workflow_version == 2,
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
            if !is_media && !is_sidecar {
                continue;
            }

            let delta_kind = match baseline {
                None => "ADD",
                Some(baseline) if baseline.is_missing => "REAPPEARED",
                Some(_) => "CHANGE",
            };
            let classification_started = measure_preparation.then(Instant::now);
            let classification = if is_media {
                Some(match library_kind {
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
                    _ => classify_mixed_file(&root_path, &path, &mut classification_cache).await,
                })
            } else {
                None
            };
            if let Some(started) = classification_started {
                classification_duration = classification_duration.saturating_add(started.elapsed());
            }
            classification_count = classification_count.saturating_add(1);
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
                        verify_path_after_preparation: false,
                    },
                    observed,
                    is_add,
                    None,
                    classification,
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
        let baselines = if stream_files_during_discovery && !file_paths.is_empty() {
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
                root_path,
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
        let mut directories_by_root = BTreeMap::<String, Vec<String>>::new();
        for (root_id, relative_directory) in directories {
            directories_by_root
                .entry(root_id)
                .or_default()
                .push(relative_directory);
        }
        let roots_by_id = self
            .database
            .list_library_roots_by_ids(&directories_by_root.keys().cloned().collect::<Vec<_>>())
            .await?;
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
                library_kind: &library.kind,
                preparation_concurrency,
                expected_root_identity,
            };
            match self
                .discover_scan_manifest_directory_group_batches(context, &relative_directories)
                .await
            {
                Ok(Some(discovered)) => {
                    discovered_count = discovered_count.saturating_add(
                        i64::try_from(discovered.observed_file_count).unwrap_or(i64::MAX),
                    );
                    created_items = created_items.saturating_add(discovered.created_items);
                    self.push_lite_manifest_directories(
                        manifest_id,
                        &root_id,
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
                preparation_tasks.spawn(async move {
                    prepare_manifest_delta(
                        ManifestFilePreparationContext {
                            scanner,
                            root,
                            root_path,
                            expected_root_device,
                            expected_root_inode,
                            verify_path_after_preparation: true,
                        },
                        delta,
                        classification,
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
        let job = self
            .database
            .find_scan_job(job_id)
            .await?
            .ok_or(ScanJobError::JobNotFound)?;
        let Some(manifest) = self.database.get_scan_manifest_by_job(job_id).await? else {
            return Ok(false);
        };
        if manifest.workflow_version != 2 || manifest.discovery_format_version != 3 {
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
                            page_size: MANIFEST_STREAMED_INDEX_BATCH_SIZE,
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
            regular_tasks.spawn(async move {
                let report = match classification {
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

        let mut grouped_regular_works = Vec::<Vec<(usize, ReconciliationRegularWork)>>::new();
        let mut regular_group_indexes = HashMap::<String, usize>::new();
        for (regular_index, work) in regular_works.iter().cloned().enumerate() {
            let group_key = reconciliation_regular_group_key(
                &work.root.id,
                &work.path,
                work.classification,
                work.index,
            );
            let group_index = *regular_group_indexes.entry(group_key).or_insert_with(|| {
                grouped_regular_works.push(Vec::new());
                grouped_regular_works.len() - 1
            });
            grouped_regular_works[group_index].push((regular_index, work));
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
                for (regular_index, work) in group {
                    let root = work.root;
                    let path = work.path;
                    let report = match work.classification {
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
        let Some(manifest) = self.database.get_scan_manifest_by_job(job_id).await? else {
            return Ok(());
        };
        if manifest.workflow_version != 2 || manifest.discovery_format_version != 3 {
            return Ok(());
        }
        for root in self
            .database
            .list_scan_manifest_postprocessing_roots(&manifest.id)
            .await?
        {
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
                    .mark_scan_manifest_root_unavailable(&manifest.id, &root.library_root_id)
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

    fn start_local_metadata_worker(&self, scan_job_id: &str) -> LocalMetadataWorkerHandle {
        let (stop, mut stop_receiver) = watch::channel(false);
        let database = self.database.clone();
        let people = self.people.clone();
        let local_nfo = self.local_nfo.clone();
        let home = self.home.clone();
        let target_root_validator = self.clone();
        let scan_job_id = scan_job_id.to_owned();
        let notifications = Arc::clone(&self.metadata_notifications);
        let notify = Arc::new(Notify::new());
        if let Ok(mut registry) = notifications.lock() {
            registry.insert(scan_job_id.clone(), Arc::clone(&notify));
        }
        let worker_job_id = scan_job_id.clone();
        let task = tokio::spawn(async move {
            let enricher = MetadataEnricher::new(database.clone());
            let enricher = match people {
                Some(people) => enricher.with_people(people),
                None => enricher,
            };
            let enricher = match local_nfo {
                Some(local_nfo) => enricher.with_nfo_store(local_nfo),
                None => enricher,
            };

            loop {
                if *stop_receiver.borrow() {
                    return;
                }
                let notified = notify.notified();
                let job = match database.find_scan_job(&worker_job_id).await {
                    Ok(Some(job)) => job,
                    Ok(None) => return,
                    Err(error) => {
                        tracing::warn!(
                            scan_job_id = %worker_job_id,
                            %error,
                            "local metadata worker could not load scan job; retrying"
                        );
                        tokio::select! {
                            changed = stop_receiver.changed() => {
                                if changed.is_err() || *stop_receiver.borrow() {
                                    return;
                                }
                            }
                            _ = tokio::time::sleep(LOCAL_METADATA_IDLE_FALLBACK) => {}
                        }
                        continue;
                    }
                };
                if matches!(job.status.as_str(), "FAILED" | "CANCELLED") {
                    return;
                }
                let pending = match database
                    .has_pending_scan_job_metadata_targets(&worker_job_id)
                    .await
                {
                    Ok(pending) => pending,
                    Err(error) => {
                        tracing::warn!(
                            scan_job_id = %worker_job_id,
                            %error,
                            "local metadata worker could not check pending targets"
                        );
                        false
                    }
                };
                if pending {
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
                        .enrich_scan_job_targets(&worker_job_id, LOCAL_METADATA_BATCH_SIZE)
                        .await
                    {
                        Ok(report) if report.items_processed > 0 => {
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
                tokio::select! {
                    changed = stop_receiver.changed() => {
                        if changed.is_err() || *stop_receiver.borrow() {
                            return;
                        }
                    }
                    _ = notified => {}
                    _ = tokio::time::sleep(LOCAL_METADATA_IDLE_FALLBACK) => {}
                }
            }
        });
        LocalMetadataWorkerHandle {
            stop,
            task,
            job_id: scan_job_id,
            notifications,
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
        while self
            .database
            .has_pending_scan_job_metadata_targets(scan_job_id)
            .await?
        {
            if let Some(notify) = &notify {
                let notified = notify.notified();
                tokio::select! {
                    _ = notified => {}
                    _ = tokio::time::sleep(LOCAL_METADATA_IDLE_FALLBACK) => {}
                }
            } else {
                tokio::time::sleep(LOCAL_METADATA_IDLE_FALLBACK).await;
            }
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
        let defer_local_metadata_worker = self
            .database
            .get_scan_manifest_by_job(job_id)
            .await?
            .is_some_and(|manifest| {
                manifest.workflow_version == 2 && manifest.discovery_format_version == 3
            });
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
                    local_metadata_worker = Some(self.start_local_metadata_worker(job_id));
                }
                drop(scan_permit);
                if self.cancellation_requested_in_memory(job_id) {
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    self.cancel_running_job(job_id).await?;
                    return Ok(());
                }
                if incremental {
                    if let Err(error) = self.wait_for_local_metadata(job_id).await {
                        Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                        return Err(error);
                    }
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    self.run_probe_after_scan(job_id, probe).await?;
                    self.run_thumbnails_after_incremental_scan(job_id, thumbnails)
                        .await?;
                    for target_type in ["SOURCE", "ITEM"] {
                        self.database
                            .skip_pending_scan_job_target_stage(job_id, target_type, "THUMBNAIL")
                            .await?;
                    }
                    self.database
                        .clear_completed_scan_job_targets(job_id)
                        .await?;
                    let strm_probe_scheduled = if completed_job.auto_metadata_match {
                        if let Some(metadata) = metadata {
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
                if let Err(error) = self.wait_for_local_metadata(job_id).await {
                    Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                    return Err(error);
                }
                Self::stop_local_metadata_worker(&mut local_metadata_worker).await;
                self.update_activity(job_id, Some("媒体探测"), "POSTPROCESSING")
                    .await?;
                if defer_local_metadata_worker {
                    self.verify_manifest_postprocessing_target_roots(job_id)
                        .await?;
                }
                self.run_probe_after_scan(job_id, probe).await?;
                self.update_activity(job_id, Some("本地元数据"), "POSTPROCESSING")
                    .await?;
                if defer_local_metadata_worker {
                    self.verify_manifest_postprocessing_target_roots(job_id)
                        .await?;
                }
                self.run_metadata_after_scan(job_id).await?;
                self.update_activity(job_id, Some("媒体库封面"), "POSTPROCESSING")
                    .await?;
                self.run_auto_library_cover_after_scan(job_id).await?;
                self.update_activity(job_id, Some("视频缩略图"), "POSTPROCESSING")
                    .await?;
                if defer_local_metadata_worker {
                    self.verify_manifest_postprocessing_target_roots(job_id)
                        .await?;
                }
                self.run_thumbnails_after_scan(job_id, thumbnails).await?;
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
                if completed_job.auto_metadata_match {
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
                tokio::time::sleep(Duration::from_millis(10)).await;
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
                tokio::time::sleep(Duration::from_millis(10)).await;
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
            tokio::time::sleep(Duration::from_millis(10)).await;
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
        let job_id = job.id.clone();
        tokio::spawn(async move {
            metadata.run(&job_id).await;
        });
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
        if !library.realtime_metadata_auto_match_enabled || library.scraper_id.as_deref().is_none()
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
            let job_id = job.id.clone();
            let worker = metadata.clone();
            tokio::spawn(async move {
                worker.run(&job_id).await;
            });
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
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Ok(());
        };
        if self.database.find_library(&job.library_id).await?.is_none() {
            return Ok(());
        }
        let enricher = MetadataEnricher::new(self.database.clone());
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
        let _ = self
            .database
            .append_scan_job_event(NewScanJobEvent {
                id: &id,
                job_id,
                level,
                event_code,
                message,
                details_json,
            })
            .await;
        self.admin_events.publish(AdminEventScope::Jobs);
    }
}

type MoviePreparationOutput = (
    usize,
    String,
    StoredReconciliationScanEntry,
    Option<NewMovieFile>,
);
type MoviePreparationTask = Result<MoviePreparationOutput, ScannerError>;
type MovieFingerprintTask = Result<(usize, Option<(String, ScanReport)>), ScannerError>;
type EpisodeQuickResult = Option<(String, ScanReport, Option<(String, String)>)>;
type EpisodeFingerprintTask = Result<(usize, EpisodeQuickResult), ScannerError>;
type ReconciliationFingerprintTask = Result<(usize, Option<(String, ScanReport)>), ScannerError>;
type ReconciliationRegularTask = Result<(usize, ScanReport), ScannerError>;
type ReconciliationRegularGroupTask = Result<Vec<(usize, ScanReport)>, ScannerError>;

fn reconciliation_regular_group_key(
    root_id: &str,
    path: &Path,
    classification: MixedClassification,
    fallback_index: usize,
) -> String {
    let parsed_key = match classification {
        MixedClassification::Movie => path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(parse_movie_filename)
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
    results: &mut [Option<(String, ScanReport)>],
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanJob {
    pub id: String,
    pub library_id: String,
    pub job_type: String,
    pub status: String,
    pub generation: String,
    pub cursor: Option<String>,
    pub processed_count: i64,
    pub total_count: i64,
    pub discovery_completed: bool,
    pub cancel_requested: bool,
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub current_item: Option<String>,
    pub scan_phase: String,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanBatchReport {
    pub status: String,
    pub processed: usize,
    pub created_items: usize,
    pub completed: bool,
}

#[derive(Debug)]
pub enum ScanJobError {
    LibraryNotFound,
    ItemNotFound,
    JobNotFound,
    NoChanges,
    AlreadyActive(String),
    InvalidBatchSize,
    ScanLockClosed,
    Scanner(ScannerError),
    Storage(StorageError),
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
            Self::Scanner(error) => error.code(),
            Self::Storage(_) => "STORAGE_ERROR",
        }
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedMovieFilename {
    pub title: String,
    pub sort_title: String,
    pub production_year: Option<i32>,
    pub edition_name: Option<String>,
    pub quality_label: Option<String>,
    pub provider_ids: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedEpisodeFilename {
    pub title: String,
    pub sort_title: String,
    pub production_year: Option<i32>,
    pub season: u32,
    pub episode: u32,
    pub absolute_number: Option<u32>,
    pub edition_name: Option<String>,
    pub quality_label: Option<String>,
    pub provider_ids: BTreeMap<String, String>,
}

#[derive(Clone, Copy)]
enum MixedClassification {
    Movie,
    Episode,
    Unresolved,
}

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

#[derive(Default)]
struct MixedClassificationCache {
    nfo_exists: HashMap<PathBuf, bool>,
    nfo_roots: HashMap<(PathBuf, String), bool>,
}

async fn classify_mixed_file(
    root: &Path,
    path: &Path,
    cache: &mut MixedClassificationCache,
) -> MixedClassification {
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return MixedClassification::Unresolved;
    };
    if parse_episode_filename(file_name).is_some() {
        return MixedClassification::Episode;
    }
    let series_nfo = path
        .strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .map(|first| root.join(first.as_os_str()).join("tvshow.nfo"));
    if let Some(series_nfo) = series_nfo
        && cached_nfo_root_is(cache, &series_nfo, "tvshow").await
    {
        return MixedClassification::Unresolved;
    }
    let movie_nfo = if let Some(candidate) =
        path.parent().map(|directory| directory.join("movie.nfo"))
        && cached_nfo_exists(cache, &candidate).await
    {
        Some(candidate)
    } else {
        let candidate = path.with_extension("nfo");
        cached_nfo_exists(cache, &candidate)
            .await
            .then_some(candidate)
    };
    if let Some(movie_nfo) = movie_nfo
        && cached_nfo_root_is(cache, &movie_nfo, "movie").await
    {
        return MixedClassification::Movie;
    }
    if parse_movie_filename(file_name).is_some_and(|parsed| parsed.production_year.is_some()) {
        MixedClassification::Movie
    } else {
        MixedClassification::Unresolved
    }
}

async fn cached_nfo_exists(cache: &mut MixedClassificationCache, path: &Path) -> bool {
    if let Some(exists) = cache.nfo_exists.get(path) {
        return *exists;
    }
    let exists = fs::metadata(path)
        .await
        .is_ok_and(|metadata| metadata.is_file());
    cache.nfo_exists.insert(path.to_owned(), exists);
    exists
}

async fn cached_nfo_root_is(
    cache: &mut MixedClassificationCache,
    path: &Path,
    expected: &str,
) -> bool {
    let key = (path.to_owned(), expected.to_owned());
    if let Some(is_expected) = cache.nfo_roots.get(&key) {
        return *is_expected;
    }
    let is_expected = nfo_root_is(path, expected).await;
    cache.nfo_roots.insert(key, is_expected);
    is_expected
}

async fn nfo_root_is(path: &Path, expected: &str) -> bool {
    let Ok(bytes) = fs::read(path).await else {
        return false;
    };
    let mut reader = Reader::from_reader(bytes.as_slice());
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event)) => {
                return event
                    .name()
                    .as_ref()
                    .eq_ignore_ascii_case(expected.as_bytes());
            }
            Ok(Event::Eof) | Err(_) => return false,
            Ok(_) => buffer.clear(),
        }
    }
}

pub fn parse_movie_filename(filename: &str) -> Option<ParsedMovieFilename> {
    parse_media_name(filename, MediaKind::Movie).map(|parsed| ParsedMovieFilename {
        title: parsed.title,
        sort_title: parsed.sort_title,
        production_year: parsed.production_year,
        edition_name: parsed.edition_name,
        quality_label: parsed.quality_label,
        provider_ids: parsed.provider_ids,
    })
}

fn movie_provider_ids(
    path: &Path,
    file_provider_ids: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut provider_ids = file_provider_ids.clone();
    for (provider, provider_id) in movie_folder_provider_ids(path) {
        provider_ids.entry(provider).or_insert(provider_id);
    }
    provider_ids
}

fn movie_folder_provider_ids(path: &Path) -> BTreeMap<String, String> {
    let Some(folder_name) = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
    else {
        return BTreeMap::new();
    };
    parse_media_name(folder_name, MediaKind::Movie)
        .map(|folder| folder.provider_ids)
        .unwrap_or_default()
}

fn provider_ids_json(provider_ids: &BTreeMap<String, String>) -> Option<String> {
    (!provider_ids.is_empty())
        .then(|| serde_json::to_string(provider_ids).unwrap_or_else(|_| "{}".to_owned()))
}

pub fn parse_episode_filename(filename: &str) -> Option<ParsedEpisodeFilename> {
    let parsed = parse_media_name(filename, MediaKind::Episode)?;
    let season = parsed.season?;
    let episode = parsed.episode?;
    let title = if parsed.title.is_empty() {
        format!("Episode {episode:02}")
    } else {
        parsed.title
    };
    Some(ParsedEpisodeFilename {
        title,
        sort_title: parsed.sort_title,
        production_year: parsed.production_year,
        season,
        episode,
        absolute_number: parsed.absolute_number,
        edition_name: parsed.edition_name,
        quality_label: parsed.quality_label,
        provider_ids: parsed.provider_ids,
    })
}

fn clean_hierarchy_title(value: &str) -> String {
    clean_title(value)
}

#[derive(Debug, Eq, PartialEq)]
struct EpisodeHierarchy {
    series_path: String,
    series_title: String,
    production_year: Option<i32>,
    provider_ids: BTreeMap<String, String>,
    season_number: u32,
}

struct EnsuredEpisodeHierarchy {
    episode_id: String,
    created_items: usize,
    episode_created: bool,
}

fn episode_hierarchy(relative_path: &str, parsed: &ParsedEpisodeFilename) -> EpisodeHierarchy {
    let components = relative_path
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let directories = components
        .split_last()
        .map(|(_, directories)| directories)
        .unwrap_or(&[]);
    let season_directory_index = directories
        .iter()
        .rposition(|value| parse_season_directory_name(value).is_some());
    let series_components = season_directory_index
        .map(|index| &directories[..index])
        .unwrap_or(directories);
    let series_path = if series_components.is_empty() {
        "Series".to_owned()
    } else {
        series_components.join("/")
    };
    let parsed_series = series_components
        .last()
        .and_then(|value| parse_media_name(value, MediaKind::Series));
    let series_title = parsed_series
        .as_ref()
        .map(|value| value.title.clone())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Series".to_owned());
    let production_year = parsed_series
        .as_ref()
        .and_then(|value| value.production_year)
        .or(parsed.production_year);
    let provider_ids = parsed_series
        .map(|value| value.provider_ids.clone())
        .filter(|provider_ids| !provider_ids.is_empty())
        .unwrap_or_else(|| parsed.provider_ids.clone());
    let season_number = season_directory_number(directories).unwrap_or(parsed.season);
    EpisodeHierarchy {
        series_path,
        series_title,
        production_year,
        provider_ids,
        season_number,
    }
}

fn legacy_series_identity(
    root: &StoredLibraryRoot,
    hierarchy: &EpisodeHierarchy,
) -> Option<String> {
    hierarchy
        .series_path
        .split('/')
        .next()
        .filter(|component| !component.is_empty())
        .map(|component| format!("series:{}:{component}", root.id))
}

fn season_directory_number(components: &[&str]) -> Option<u32> {
    components
        .iter()
        .rev()
        .find_map(|value| parse_season_directory_name(value))
}

fn parse_season_directory_name(value: &str) -> Option<u32> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized == "specials" {
        return Some(0);
    }
    let digits = normalized
        .strip_prefix("season")
        .or_else(|| normalized.strip_prefix('s'))?
        .trim();
    let digits = if let Some((prefix, suffix)) = digits.split_once('(') {
        suffix.strip_suffix(')').filter(|value| {
            !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
        })?;
        prefix.trim()
    } else {
        digits
    };
    digits.parse::<u32>().ok()
}

struct FileBatchWalker {
    directories: Vec<PathBuf>,
    current: Option<(PathBuf, fs::ReadDir)>,
}

impl FileBatchWalker {
    fn new(root: &Path) -> Self {
        Self {
            directories: vec![root.to_owned()],
            current: None,
        }
    }

    async fn next_batch(
        &mut self,
        batch_size: usize,
    ) -> Result<Option<Vec<PathBuf>>, ScannerError> {
        if batch_size == 0 {
            return Ok(None);
        }
        let mut files = Vec::with_capacity(batch_size);
        while files.len() < batch_size {
            if self.current.is_none() {
                let Some(directory) = self.directories.pop() else {
                    break;
                };
                let entries =
                    fs::read_dir(&directory)
                        .await
                        .map_err(|source| ScannerError::Io {
                            path: directory.clone(),
                            source,
                        })?;
                self.current = Some((directory, entries));
            }

            let Some((directory, entries)) = self.current.as_mut() else {
                continue;
            };
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    let path = entry.path();
                    let file_type = entry.file_type().await.map_err(|source| ScannerError::Io {
                        path: path.clone(),
                        source,
                    })?;
                    if file_type.is_file() && is_supported_movie_file(&path) {
                        files.push(path);
                    } else if file_type.is_dir() {
                        self.directories.push(path);
                    }
                }
                Ok(None) => self.current = None,
                Err(source) => {
                    return Err(ScannerError::Io {
                        path: directory.clone(),
                        source,
                    });
                }
            }
        }
        if files.is_empty() {
            Ok(None)
        } else {
            files.sort();
            Ok(Some(files))
        }
    }
}

fn is_supported_movie_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mkv" | "mp4" | "strm"
            )
        })
}

fn is_supported_sidecar_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "nfo" | "jpg" | "jpeg" | "png" | "webp" | "edl" | "xml"
            )
        })
}

fn is_strm_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"))
}

async fn read_strm_target(path: &Path) -> Result<StrmTarget, ScannerError> {
    let contents = fs::read_to_string(path)
        .await
        .map_err(|source| ScannerError::Io {
            path: path.to_owned(),
            source,
        })?;
    Ok(classify_strm_target(&contents))
}

fn strm_target_kind_name(target: &StrmTarget) -> &'static str {
    match target.kind {
        StrmTargetKind::Empty => "EMPTY",
        StrmTargetKind::Url => "URL",
        StrmTargetKind::Path => "PATH",
        StrmTargetKind::Smb | StrmTargetKind::Ftp | StrmTargetKind::Unsupported => "OPAQUE",
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
            Self::Storage(_) => "STORAGE_ERROR",
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
mod tests {
    use super::{
        MANIFEST_DISCOVERY_BATCH_SIZE, MANIFEST_STREAMED_INDEX_BATCH_SIZE, ManifestDirectoryReader,
        ManifestRemovalOutcome, ManifestRootDiscoveryContext, MixedClassification,
        MixedClassificationCache, NewScanManifestDiscoveryChunk, NewScanManifestEntry,
        PendingManifestDirectoryChunk, ScanJobService, ScannerError,
        classify_manifest_removal_outcomes, classify_mixed_file, configured_scan_concurrency,
        is_lite_manifest_discovery, manifest_root_identity_matches, media_source_folder,
        normalize_incremental_path, read_manifest_strm_target, safe_scan_activity_label,
        stat_manifest_directory_file_batch_sync, stat_manifest_relative_file_sync,
        stat_manifest_root_sync,
    };

    #[test]
    fn configured_scan_concurrency_prefers_global_override_then_library_value() {
        assert_eq!(configured_scan_concurrency(Some(8), Some(4), 16), 8);
        assert_eq!(configured_scan_concurrency(None, Some(4), 16), 4);
        assert_eq!(configured_scan_concurrency(None, None, 16), 16);
    }

    #[test]
    fn lite_manifest_discovery_requires_the_streamed_v3_contract() {
        assert!(is_lite_manifest_discovery(2, 3, "LITE"));
        assert!(!is_lite_manifest_discovery(1, 3, "LITE"));
        assert!(!is_lite_manifest_discovery(2, 2, "LITE"));
        assert!(!is_lite_manifest_discovery(2, 3, "PERSISTED"));
    }

    #[test]
    fn manifest_discovery_budget_stays_within_streamed_file_budget() {
        assert_eq!(MANIFEST_DISCOVERY_BATCH_SIZE, 80);
        assert!(MANIFEST_DISCOVERY_BATCH_SIZE * 100 <= MANIFEST_STREAMED_INDEX_BATCH_SIZE);
    }

    #[tokio::test]
    async fn manifest_single_chunk_commit_rejects_replaced_empty_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            application::libraries::LibraryService, config::Config, library::LibraryKind,
            storage::Database,
        };

        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let root_path = temp_dir.path().join("Movies");
        let bucket_path = root_path.join("Bucket");
        std::fs::create_dir_all(&bucket_path)?;
        let root_path = std::fs::canonicalize(root_path)?;
        let bucket_path = root_path.join("Bucket");
        let root_record = libraries
            .add_root(library.id, root_path.to_str().ok_or("non-utf8 path")?)
            .await?
            .root;
        let root = database
            .find_library_root(&root_record.id.to_string())
            .await?
            .ok_or("stored library root is missing")?;

        let reader = ManifestDirectoryReader::open(&root_path, "Bucket")?;
        let root_observation = reader.root_observation.clone();
        let directory_observation = reader.directory_observation.clone();
        let (reader, batch) = reader.next_batch(16)?;
        drop(reader);
        std::fs::rename(&bucket_path, root_path.join("Bucket.old"))?;
        std::fs::create_dir(&bucket_path)?;

        let service = ScanJobService::new(database);
        let cancellation = std::sync::atomic::AtomicBool::new(false);
        let child_directories = batch.child_directories;
        let entries = batch.entries;
        let incomplete_chunk = PendingManifestDirectoryChunk {
            relative_directory: "Bucket".to_owned(),
            root_observation: root_observation.clone(),
            directory_observation: directory_observation.clone(),
            child_directories: child_directories.clone(),
            entries: entries.clone(),
            completed_directory: None,
        };
        let result = service
            .commit_pending_manifest_directory_chunks(
                ManifestRootDiscoveryContext {
                    job_id: "replacement-test-job",
                    manifest_id: "replacement-test-manifest",
                    root: &root,
                    cancellation: &cancellation,
                    stream_files_during_discovery: true,
                    library_kind: "MOVIE",
                    preparation_concurrency: 1,
                    expected_root_identity: root_observation.device.zip(root_observation.inode),
                },
                &[incomplete_chunk],
            )
            .await;
        assert!(
            matches!(result, Err(ScannerError::RootIdentityChanged(_))),
            "incomplete unchanged-only chunks must not checkpoint after directory replacement"
        );

        let batched_chunk = PendingManifestDirectoryChunk {
            relative_directory: "Bucket".to_owned(),
            root_observation: root_observation.clone(),
            directory_observation: directory_observation.clone(),
            child_directories: child_directories.clone(),
            entries: entries.clone(),
            completed_directory: Some("Bucket".to_owned()),
        };
        let result = service
            .commit_pending_manifest_directory_chunks(
                ManifestRootDiscoveryContext {
                    job_id: "replacement-test-job",
                    manifest_id: "replacement-test-manifest",
                    root: &root,
                    cancellation: &cancellation,
                    stream_files_during_discovery: true,
                    library_kind: "MOVIE",
                    preparation_concurrency: 1,
                    expected_root_identity: root_observation.device.zip(root_observation.inode),
                },
                &[batched_chunk],
            )
            .await;
        assert!(
            matches!(result, Err(ScannerError::RootIdentityChanged(_))),
            "batched discovery must reject an empty directory replacement"
        );

        let single_chunk = NewScanManifestDiscoveryChunk {
            manifest_id: "replacement-test-manifest",
            job_id: "replacement-test-job",
            library_root_id: &root.id,
            child_directories: &child_directories,
            entries: &entries,
            positive_indexes: &[],
            unchanged_paths: &[],
            seen_filesystem_entries: &[],
            completed_directory: Some("Bucket"),
        };
        let result = service
            .commit_scan_manifest_discovery_chunk(
                &single_chunk,
                &cancellation,
                true,
                root_path,
                root_observation,
                directory_observation,
            )
            .await;
        assert!(
            matches!(result, Err(ScannerError::RootIdentityChanged(_))),
            "single-directory discovery must reject an empty directory replacement"
        );
        Ok(())
    }

    #[tokio::test]
    async fn full_manifest_scan_refreshes_home_once_before_postprocessing()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            application::{
                access::{AccessPrincipal, MediaAccessService},
                admin_events::{UserEventHub, UserEventScope},
                catalog::CatalogService,
                home::HomeService,
                libraries::LibraryService,
                setup::SetupService,
                webhooks::WebhookService,
            },
            config::{Config, DatabaseConfiguration, PostgresConnection},
            library::LibraryKind,
            storage::Database,
        };

        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let postgres_backend =
            std::env::var("LUX_SCAN_TEST_BACKEND").is_ok_and(|backend| backend == "postgres");
        let database = if postgres_backend {
            let database_name = std::env::var("POSTGRES_TEST_DATABASE")?;
            if matches!(
                database_name.as_str(),
                "postgres" | "template0" | "template1"
            ) {
                return Err("POSTGRES_TEST_DATABASE must name a disposable empty database".into());
            }
            Database::connect_with_configuration(
                &config,
                &DatabaseConfiguration::Postgres(PostgresConnection {
                    host: std::env::var("POSTGRES_TEST_HOST")
                        .unwrap_or_else(|_| "127.0.0.1".to_owned()),
                    port: std::env::var("POSTGRES_TEST_PORT")
                        .unwrap_or_else(|_| "55432".to_owned())
                        .parse()?,
                    database: database_name,
                    username: std::env::var("POSTGRES_TEST_USER")
                        .unwrap_or_else(|_| "lux".to_owned()),
                    password: std::env::var("POSTGRES_TEST_PASSWORD")
                        .unwrap_or_else(|_| "lux-test-password".to_owned()),
                    ssl_mode: "disable".to_owned(),
                }),
            )
            .await?
        } else {
            Database::connect(&config).await?
        };
        let admin = SetupService::new(database.clone())?
            .complete("Admin", "Admin", "correct password")
            .await?;
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, true)
            .await?;
        let root = temp_dir.path().join("Movies");
        tokio::fs::create_dir_all(&root).await?;
        tokio::fs::write(root.join("Home.Refresh.2025.mkv"), b"fixture").await?;
        libraries
            .add_root(library.id, root.to_str().ok_or("non-UTF-8 root path")?)
            .await?;

        let home = HomeService::new(
            CatalogService::new(database.clone(), MediaAccessService::new(database.clone())),
            libraries,
        );
        let principal = AccessPrincipal::new(admin.id, true);
        let library_ids = [library.id.to_string()];
        let before = home.snapshot(principal, library_ids.to_vec()).await?;
        assert_eq!(before.recently_added.total, 0);

        let user_events = UserEventHub::new();
        let mut receiver = user_events.subscribe();
        let webhooks = WebhookService::new(database.clone(), config.config_dir.clone())?;
        webhooks
            .create_destination(
                "Scan completion test",
                "http://127.0.0.1:8098/hook",
                true,
                true,
                &["SCAN_COMPLETED".to_owned()],
                None,
            )
            .await?;
        let jobs = ScanJobService::new(database.clone())
            .with_home(home.clone())
            .with_user_events(user_events)
            .with_webhooks(webhooks);
        let job = jobs.create_movie_scan_job(library.id).await?;
        loop {
            let report = jobs.run_batch(&job.id, 100).await?;
            if report.completed {
                assert_eq!(report.status, "COMPLETED");
                break;
            }
        }

        let stored_job = database
            .find_scan_job(&job.id)
            .await?
            .ok_or("scan job disappeared")?;
        assert_eq!(stored_job.scan_phase, "POSTPROCESSING");
        assert_eq!(receiver.try_recv(), Ok(UserEventScope::Home));
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        let dedupe_placeholder = if postgres_backend { "$1" } else { "?" };
        let scan_completed_query = format!(
            "SELECT COUNT(*) FROM notification_events
             WHERE event_type = 'SCAN_COMPLETED' AND dedupe_key = {dedupe_placeholder}"
        );
        let scan_completed_events: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(scan_completed_query))
                .bind(format!("scan:{}:SCAN_COMPLETED", job.id))
                .fetch_one(database.pool())
                .await?;
        assert_eq!(scan_completed_events, 1);
        let after_index = home.snapshot(principal, library_ids.to_vec()).await?;
        assert_eq!(after_index.recently_added.total, 1);

        let repeated_postprocessing_batch = jobs.run_batch(&job.id, 100).await?;
        assert_eq!(repeated_postprocessing_batch.status, "COMPLETED");

        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        Ok(())
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    #[test]
    fn manifest_directory_reader_enumerates_from_the_open_directory_handle()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp_dir.path())?;
        std::fs::create_dir(root.join("Nested"))?;
        std::fs::write(root.join("Example.Movie.2025.mkv"), b"fixture")?;

        let reader = ManifestDirectoryReader::open(&root, "")?;
        let (_reader, batch) = reader.next_batch(500)?;

        assert!(batch.completed);
        assert_eq!(batch.child_directories, vec!["Nested"]);
        assert!(
            batch
                .entries
                .iter()
                .any(|entry| entry.relative_path == "Example.Movie.2025.mkv")
        );
        assert!(
            batch
                .entries
                .iter()
                .any(|entry| entry.relative_path.is_empty())
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn manifest_directory_batch_stat_rejects_replaced_parent_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp_dir.path())?;
        let bucket = root.join("Bucket");
        std::fs::create_dir(&bucket)?;
        std::fs::write(bucket.join("Example.Movie.2025.mkv"), b"fixture")?;

        let reader = ManifestDirectoryReader::open(&root, "Bucket")?;
        let root_observation = reader.root_observation.clone();
        let directory_observation = reader.directory_observation.clone();
        let (_reader, batch) = reader.next_batch(500)?;
        let file_observation = batch
            .entries
            .into_iter()
            .find(|entry| entry.entry_kind == "FILE")
            .ok_or("file observation is missing")?;

        std::fs::rename(&bucket, root.join("Bucket.old"))?;
        std::fs::create_dir(&bucket)?;
        std::fs::write(bucket.join("Example.Movie.2025.mkv"), b"replacement").map(|_| ())?;

        let result = stat_manifest_directory_file_batch_sync(
            &root,
            &root_observation,
            &directory_observation,
            &[file_observation],
        );
        assert!(matches!(result, Err(ScannerError::RootIdentityChanged(_))));
        Ok(())
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    #[tokio::test]
    async fn manifest_strm_read_rejects_same_inode_replaced_by_symlink()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let library_path = temp_dir.path().join("Library");
        std::fs::create_dir(&library_path)?;
        let root = std::fs::canonicalize(library_path)?;
        let strm_path = root.join("Example.Movie.2025.strm");
        std::fs::write(&strm_path, "https://example.invalid/video")?;
        let external_path = temp_dir.path().join("Moved.strm");
        let root_observation = stat_manifest_root_sync(&root)?;
        let observation = stat_manifest_relative_file_sync(
            &root,
            "Example.Movie.2025.strm",
            root_observation.device,
            root_observation.inode,
        )?
        .expect("STRM file observation");

        std::fs::rename(&strm_path, &external_path)?;
        std::os::unix::fs::symlink(&external_path, &strm_path)?;

        let result = read_manifest_strm_target(
            root,
            "Example.Movie.2025.strm".to_owned(),
            root_observation.device,
            root_observation.inode,
            observation,
        )
        .await;
        assert!(result.is_err(), "manifest read must not follow the symlink");
        Ok(())
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))]
    #[tokio::test]
    async fn manifest_strm_read_rejects_oversized_target_contents()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let library_path = temp_dir.path().join("Library");
        std::fs::create_dir(&library_path)?;
        let root = std::fs::canonicalize(library_path)?;
        let strm_path = root.join("Example.Movie.2025.strm");
        std::fs::write(
            &strm_path,
            format!("https://example.invalid/{}", "x".repeat(1024 * 1024)),
        )?;
        let root_observation = stat_manifest_root_sync(&root)?;
        let observation = stat_manifest_relative_file_sync(
            &root,
            "Example.Movie.2025.strm",
            root_observation.device,
            root_observation.inode,
        )?
        .expect("STRM file observation");

        let result = read_manifest_strm_target(
            root,
            "Example.Movie.2025.strm".to_owned(),
            root_observation.device,
            root_observation.inode,
            observation,
        )
        .await;

        assert!(matches!(
            result,
            Err(ScannerError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::InvalidData
        ));
        Ok(())
    }

    #[test]
    fn manifest_removal_confirmation_rejects_reappeared_paths() {
        let decision = classify_manifest_removal_outcomes(&[
            ("reappeared".to_owned(), ManifestRemovalOutcome::Present),
            ("still-missing".to_owned(), ManifestRemovalOutcome::Missing),
        ]);

        assert_eq!(decision.confirmed_missing_ids, vec!["still-missing"]);
        assert_eq!(decision.unstable_ids, vec!["reappeared"]);
        assert!(!decision.root_identity_lost);
    }

    #[test]
    fn manifest_removal_io_error_invalidates_all_sibling_deletes() {
        let decision = classify_manifest_removal_outcomes(&[
            ("missing".to_owned(), ManifestRemovalOutcome::Missing),
            ("unreadable".to_owned(), ManifestRemovalOutcome::PathIoError),
        ]);

        assert!(decision.confirmed_missing_ids.is_empty());
        assert_eq!(decision.unstable_ids, vec!["missing", "unreadable"]);
        assert!(!decision.root_identity_lost);
    }

    #[test]
    fn manifest_root_without_stable_identity_is_never_authoritative() {
        let observed = NewScanManifestEntry {
            relative_path: String::new(),
            entry_kind: "DIRECTORY".to_owned(),
            size: 0,
            modified_at: 0,
            device: None,
            inode: None,
            fingerprint: Vec::new(),
        };

        assert!(!manifest_root_identity_matches(None, None, &observed));
    }

    #[test]
    fn media_source_folder_uses_the_source_parent_directory() {
        assert_eq!(
            media_source_folder("Movies/Dune/Dune.2021.mkv").unwrap(),
            "Movies/Dune"
        );
        assert_eq!(media_source_folder("Dune.2021.mkv").unwrap(), ".");
    }

    #[test]
    fn normalize_incremental_path_canonicalizes_external_separators() {
        assert_eq!(
            normalize_incremental_path(r"Movies\Dune\Dune.2021.mkv").unwrap(),
            "Movies/Dune/Dune.2021.mkv"
        );
    }

    #[test]
    fn scan_activity_label_uses_only_the_relative_basename() {
        assert_eq!(
            safe_scan_activity_label("Movies/Dune/Dune.2021.mkv").as_deref(),
            Some("Dune.2021.mkv")
        );
        assert_eq!(
            safe_scan_activity_label("/private/root/Secret.strm?token=abc").as_deref(),
            Some("Secret.strm")
        );
        assert_eq!(safe_scan_activity_label("/"), None);
    }

    #[tokio::test]
    async fn mixed_classification_cache_reuses_the_series_nfo_probe() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let root = temp_dir.path();
        let series_dir = root.join("Example Show");
        tokio::fs::create_dir_all(&series_dir)
            .await
            .expect("series directory");
        tokio::fs::write(series_dir.join("tvshow.nfo"), "<tvshow />")
            .await
            .expect("series NFO");
        let first = series_dir.join("first.mkv");
        let second = series_dir.join("second.mkv");
        let mut cache = MixedClassificationCache::default();

        assert!(matches!(
            classify_mixed_file(root, &first, &mut cache).await,
            MixedClassification::Unresolved
        ));
        assert!(matches!(
            classify_mixed_file(root, &second, &mut cache).await,
            MixedClassification::Unresolved
        ));
        assert_eq!(cache.nfo_roots.len(), 1);
    }
}
