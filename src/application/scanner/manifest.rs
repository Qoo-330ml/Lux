use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock, atomic::AtomicBool},
    time::{Duration, Instant},
};

use crate::{
    application::{
        background_work::{BackgroundWorkStage, run_process_background_blocking},
        strm_target::{StrmTarget, classify_strm_target},
    },
    domain::ids::FilesystemEntryId,
    storage::{
        NewEpisodeFile, NewMovieFile, NewScanManifestEntry, NewScanManifestIndexedFile,
        NewScanManifestPositiveIndex, NewScanManifestSidecarEntry, NewScanManifestUnresolvedFile,
        StoredLibraryRoot, StoredScanManifestDelta, StoredScanManifestFilesystemBaseline,
    },
};
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

use super::{
    LibraryScanner, MANIFEST_STREAMED_ENTRY_BATCH_SIZE, MAX_STRM_TARGET_BYTES,
    ManifestFilenameInput, MixedClassification, PreparedManifestFilename, ScannerError,
    infer_sibling_movie_variant_suffix, is_strm_file, is_supported_movie_file,
    manifest_entry_observation, manifest_entry_observation_from_stat, movie_folder_provider_ids,
    prepare_manifest_filename, prepare_manifest_filename_with_variant_suffix,
};

pub(super) struct ManifestDirectoryBatch {
    pub(super) child_directories: Vec<String>,
    pub(super) entries: Vec<NewScanManifestEntry>,
    pub(super) completed: bool,
    pub(super) readdir_duration: Duration,
    pub(super) stat_duration: Duration,
    pub(super) readdir_entry_count: usize,
    pub(super) stat_entry_count: usize,
}

pub(super) fn record_manifest_scan_stage(
    phase: &'static str,
    started: Instant,
    units: u64,
    files: u64,
    directories: u64,
) {
    record_manifest_scan_stage_duration(phase, started.elapsed(), units, files, directories);
}

pub(super) fn record_manifest_scan_stage_duration(
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

pub(super) fn record_manifest_scan_activity(
    active_preparation_tasks: usize,
    active_directory_readers: usize,
) {
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

pub(super) struct PendingManifestDirectoryChunk {
    pub(super) relative_directory: String,
    pub(super) root_observation: NewScanManifestEntry,
    pub(super) directory_observation: NewScanManifestEntry,
    pub(super) child_directories: Vec<String>,
    pub(super) entries: Vec<NewScanManifestEntry>,
    pub(super) completed_directory: Option<String>,
}

#[derive(Default)]
pub(super) struct ManifestDiscoveryDirectoryResult {
    pub(super) observed_file_count: usize,
    pub(super) created_items: usize,
    pub(super) child_directories: Vec<String>,
}

#[derive(Default)]
pub(super) struct LiteManifestDiscoverySession {
    pub(super) directories: VecDeque<(String, String, bool)>,
}

pub(super) enum PreparedManifestFile {
    Movie(NewMovieFile),
    Episode(NewEpisodeFile),
    Unresolved(NewScanManifestUnresolvedFile),
    HomeVideo(NewScanManifestUnresolvedFile),
}

pub(super) enum ManifestDeltaPreparation {
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

pub(super) struct ManifestPositiveIndexSeed {
    pub(super) relative_path: String,
    pub(super) delta_kind: String,
    pub(super) base_filesystem_entry_id: Option<String>,
    pub(super) base_fingerprint: Option<Vec<u8>>,
}

// The JoinSet bounds live values to scan concurrency; boxing would allocate once per indexed
// file on the 60k-file hot path.
#[allow(clippy::large_enum_variant)]
pub(super) enum ManifestDiscoveryIndexPreparation {
    Indexed(NewScanManifestPositiveIndex),
    Unstable,
    RootIdentityChanged,
}

pub(super) struct ManifestFilePreparationContext {
    pub(super) scanner: LibraryScanner,
    pub(super) root: StoredLibraryRoot,
    pub(super) root_path: PathBuf,
    pub(super) expected_root_device: Option<i64>,
    pub(super) expected_root_inode: Option<i64>,
    pub(super) movie_folder_provider_ids_cache: Option<Arc<OnceLock<BTreeMap<String, String>>>>,
    pub(super) verify_path_after_preparation: bool,
}

#[derive(Clone, Copy)]
pub(super) struct ManifestRootDiscoveryContext<'a> {
    pub(super) job_id: &'a str,
    pub(super) manifest_id: &'a str,
    pub(super) root: &'a StoredLibraryRoot,
    pub(super) cancellation: &'a AtomicBool,
    pub(super) stream_files_during_discovery: bool,
    pub(super) skip_baseline_queries: bool,
    pub(super) library_kind: &'a str,
    pub(super) preparation_concurrency: usize,
    pub(super) expected_root_identity: Option<(i64, i64)>,
}

#[derive(Clone, Copy)]
pub(super) struct ManifestPositiveIndexPreparationContext<'a> {
    pub(super) root: &'a StoredLibraryRoot,
    pub(super) relative_directory: &'a str,
    pub(super) library_kind: &'a str,
    pub(super) preparation_concurrency: usize,
    pub(super) baselines: &'a HashMap<String, StoredScanManifestFilesystemBaseline>,
    pub(super) expected_root_identity: Option<(i64, i64)>,
    pub(super) expected_root_observation: &'a NewScanManifestEntry,
    pub(super) expected_directory_observation: &'a NewScanManifestEntry,
    pub(super) entries: &'a [NewScanManifestEntry],
    pub(super) cancellation: &'a AtomicBool,
}

#[derive(Default)]
pub(super) struct ManifestApplyPreparedFiles {
    pub(super) movie_files: Vec<NewMovieFile>,
    pub(super) episode_files: Vec<NewEpisodeFile>,
    pub(super) unresolved_files: Vec<NewScanManifestUnresolvedFile>,
    pub(super) sidecar_entries: Vec<NewScanManifestSidecarEntry>,
    pub(super) unstable_delta_ids: Vec<String>,
    pub(super) root_identity_lost: bool,
}

impl ManifestApplyPreparedFiles {
    pub(super) fn record(&mut self, preparation: ManifestDeltaPreparation) {
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
                        PreparedManifestFile::Unresolved(file)
                        | PreparedManifestFile::HomeVideo(file) => self.unresolved_files.push(file),
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
pub(super) enum ManifestRemovalOutcome {
    Missing,
    Present,
    PathIoError,
    InvalidPath,
    RootIdentityChanged,
}

#[derive(Default)]
pub(super) struct ManifestRemovalDecision {
    pub(super) confirmed_missing_ids: Vec<String>,
    pub(super) unstable_ids: Vec<String>,
    pub(super) root_identity_lost: bool,
}

pub(super) fn classify_manifest_removal_outcomes(
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
    pub(super) fn matches_observation(&self, observation: &NewScanManifestEntry) -> bool {
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
            Self::HomeVideo(file) => {
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
pub(super) struct ManifestDirectoryReader {
    pub(super) root_path: PathBuf,
    pub(super) relative_directory: String,
    pub(super) _directory: std::fs::File,
    pub(super) entries: *mut libc::DIR,
    pub(super) root_observation: NewScanManifestEntry,
    pub(super) root_observation_emitted: bool,
    pub(super) directory_observation: NewScanManifestEntry,
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
pub(super) fn open_manifest_directory_identity(
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
    pub(super) fn open(root_path: &Path, relative_directory: &str) -> Result<Self, ScannerError> {
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

    pub(super) fn next_batch(
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
                        || super::library::is_supported_incremental_sidecar_file(Path::new(name))))
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
pub(super) fn clear_manifest_errno() {
    // SAFETY: errno location is thread-local and readdir is called synchronously afterward.
    unsafe { *libc::__errno_location() = 0 };
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub(super) fn clear_manifest_errno() {
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

pub(super) async fn stat_manifest_relative_file(
    root_path: PathBuf,
    relative_path: String,
    expected_root_device: Option<i64>,
    expected_root_inode: Option<i64>,
) -> Result<Option<NewScanManifestEntry>, ScannerError> {
    let display_path = root_path.clone();
    run_process_background_blocking(BackgroundWorkStage::ManifestPreparation, move || {
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

pub(super) async fn read_manifest_strm_target(
    root_path: PathBuf,
    relative_path: String,
    expected_root_device: Option<i64>,
    expected_root_inode: Option<i64>,
    expected_observation: NewScanManifestEntry,
) -> Result<StrmTarget, ScannerError> {
    let display_path = root_path.join(&relative_path);
    run_process_background_blocking(BackgroundWorkStage::ManifestPreparation, move || {
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

pub(super) async fn stat_manifest_root(
    root_path: PathBuf,
) -> Result<NewScanManifestEntry, ScannerError> {
    let display_path = root_path.clone();
    run_process_background_blocking(BackgroundWorkStage::ManifestPreparation, move || {
        stat_manifest_root_sync(&root_path)
    })
    .await
    .map_err(|source| ScannerError::Io {
        path: display_path,
        source: std::io::Error::other(source.to_string()),
    })?
}

pub(super) fn stat_manifest_root_sync(
    root_path: &Path,
) -> Result<NewScanManifestEntry, ScannerError> {
    ManifestDirectoryReader::open(root_path, "").map(|reader| reader.root_observation.clone())
}

pub(super) fn manifest_root_identity_matches(
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
pub(super) fn stat_manifest_relative_file_sync(
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
pub(super) fn stat_manifest_relative_file_sync(
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

pub(super) fn manifest_directory_observation_matches(
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
pub(super) fn open_manifest_directory_for_final_check(
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

pub(super) async fn verify_manifest_directory_observation(
    root_path: PathBuf,
    expected_root_observation: NewScanManifestEntry,
    expected_directory_observation: NewScanManifestEntry,
) -> Result<(), ScannerError> {
    let display_path = root_path.clone();
    run_process_background_blocking(BackgroundWorkStage::ManifestPreparation, move || {
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

pub(super) async fn verify_manifest_directory_observations(
    root_path: PathBuf,
    observations: Vec<(NewScanManifestEntry, NewScanManifestEntry)>,
) -> Result<(), ScannerError> {
    let display_path = root_path.clone();
    let directory_count = observations.len();
    let started = Instant::now();
    let result =
        run_process_background_blocking(BackgroundWorkStage::ManifestPreparation, move || {
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

pub(super) fn verify_manifest_directory_observation_sync(
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

pub(super) async fn stat_manifest_directory_file_batch(
    root_path: PathBuf,
    expected_root_observation: NewScanManifestEntry,
    expected_directory_observation: NewScanManifestEntry,
    expected_files: Vec<NewScanManifestEntry>,
) -> Result<Vec<Option<NewScanManifestEntry>>, ScannerError> {
    let display_path = root_path.clone();
    let file_count = expected_files.len();
    let started = Instant::now();
    let result =
        run_process_background_blocking(BackgroundWorkStage::ManifestPreparation, move || {
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
pub(super) fn stat_manifest_directory_file_batch_sync(
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
pub(super) fn stat_manifest_directory_file_batch_sync(
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
pub(super) fn stat_manifest_directory_files_from_handle(
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
pub(super) fn stat_manifest_directory_files_from_reader(
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
pub(super) fn read_manifest_strm_target_sync(
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
    if file_size > MAX_STRM_TARGET_BYTES {
        return Err(ScannerError::Io {
            path: root_path.join(relative_path),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "STRM target exceeds the size limit",
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
        .take(u64::try_from(MAX_STRM_TARGET_BYTES.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_string(&mut contents)
        .map_err(|source| ScannerError::Io {
            path: root_path.join(relative_path),
            source,
        })?;
    if bytes_read > MAX_STRM_TARGET_BYTES {
        return Err(ScannerError::Io {
            path: root_path.join(relative_path),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "STRM target exceeds the size limit",
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
pub(super) fn read_manifest_strm_target_sync(
    _root_path: &Path,
    relative_path: &str,
    _expected_root_device: Option<i64>,
    _expected_root_inode: Option<i64>,
    _expected_observation: &NewScanManifestEntry,
) -> Result<StrmTarget, ScannerError> {
    Err(ScannerError::InvalidRelativePath(relative_path.to_owned()))
}

pub(super) fn manifest_file_observation_matches(
    expected: &NewScanManifestEntry,
    observed: &NewScanManifestEntry,
) -> bool {
    expected.relative_path == observed.relative_path
        && expected.entry_kind == observed.entry_kind
        && expected.size == observed.size
        && expected.modified_at == observed.modified_at
        && expected.device == observed.device
        && expected.inode == observed.inode
}

pub(super) fn manifest_discovery_index_from_preparation(
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
                    PreparedManifestFile::HomeVideo(file) => {
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

pub(super) async fn prepare_manifest_delta(
    context: ManifestFilePreparationContext,
    delta: StoredScanManifestDelta,
    classification: Option<MixedClassification>,
    library_kind: &str,
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
        fingerprint_pending: false,
    };
    let filename_input = if library_kind == "HOMEVIDEOS" {
        ManifestFilenameInput::HomeVideos
    } else {
        ManifestFilenameInput::LegacyMixed(classification)
    };
    prepare_manifest_observation(
        context,
        observed,
        delta_kind == "ADD",
        delta_id,
        filename_input,
    )
    .await
}

pub(super) async fn prepare_manifest_observation(
    context: ManifestFilePreparationContext,
    observed: NewScanManifestEntry,
    is_add: bool,
    delta_id: Option<String>,
    filename_input: ManifestFilenameInput,
) -> ManifestDeltaPreparation {
    let ManifestFilePreparationContext {
        scanner,
        root,
        root_path,
        expected_root_device,
        expected_root_inode,
        movie_folder_provider_ids_cache,
        verify_path_after_preparation,
    } = context;
    if observed.entry_kind != "FILE" {
        return ManifestDeltaPreparation::Unstable { delta_id };
    }
    let path = root_path.join(&observed.relative_path);
    let is_media = is_supported_movie_file(Path::new(&observed.relative_path));
    let is_sidecar =
        super::library::is_supported_incremental_sidecar_file(Path::new(&observed.relative_path));
    if !is_media && !is_sidecar {
        return ManifestDeltaPreparation::Unstable { delta_id };
    }
    let prepared_filename = if is_media {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return ManifestDeltaPreparation::Unstable { delta_id };
        };
        let inferred_suffix = if filename_input.is_movie() {
            infer_sibling_movie_variant_suffix(&path).await
        } else {
            None
        };
        let prepared_filename = if let Some(suffix) = inferred_suffix.as_deref() {
            prepare_manifest_filename_with_variant_suffix(file_name, filename_input, Some(suffix))
        } else {
            prepare_manifest_filename(file_name, filename_input)
        };
        let Some(prepared_filename) = prepared_filename else {
            return ManifestDeltaPreparation::Unstable { delta_id };
        };
        Some(prepared_filename)
    } else {
        None
    };
    let movie_folder_provider_ids = prepared_filename
        .as_ref()
        .filter(|prepared| matches!(prepared, PreparedManifestFilename::Movie(_)))
        .and(movie_folder_provider_ids_cache.as_ref())
        .map(|cache| cache.get_or_init(|| movie_folder_provider_ids(&path)));
    let prepared_file = if is_media {
        let Some(prepared_filename) = prepared_filename else {
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
        let prepared = match prepared_filename {
            PreparedManifestFilename::Movie(parsed_name) => scanner
                .prepare_manifest_movie_file(
                    &path,
                    &observed,
                    manifest_strm_target,
                    parsed_name,
                    movie_folder_provider_ids,
                )
                .await
                .map(|file| file.map(PreparedManifestFile::Movie)),
            PreparedManifestFilename::Episode(parsed_name) => scanner
                .prepare_manifest_episode_file(
                    &root.id,
                    &path,
                    &observed,
                    manifest_strm_target,
                    parsed_name,
                )
                .await
                .map(|file| file.map(PreparedManifestFile::Episode)),
            PreparedManifestFilename::Unresolved => scanner
                .prepare_manifest_unresolved_file(&root, &path, &observed, manifest_strm_target)
                .await
                .map(|file| Some(PreparedManifestFile::Unresolved(file))),
            PreparedManifestFilename::HomeVideo => scanner
                .prepare_manifest_home_video_file(&root, &path, &observed, manifest_strm_target)
                .await
                .map(|file| Some(PreparedManifestFile::HomeVideo(file))),
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
pub(super) fn open_manifest_directory_component(
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
pub(super) struct ManifestDirectoryReader {
    pub(super) root_path: PathBuf,
    pub(super) relative_directory: String,
    pub(super) directory_path: PathBuf,
    pub(super) entries: std::fs::ReadDir,
    pub(super) root_observation: NewScanManifestEntry,
    pub(super) root_observation_emitted: bool,
    pub(super) directory_observation: NewScanManifestEntry,
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
)))]
impl ManifestDirectoryReader {
    pub(super) fn open(root_path: &Path, relative_directory: &str) -> Result<Self, ScannerError> {
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

    pub(super) fn next_batch(
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
                    && (is_supported_movie_file(&path)
                        || super::library::is_supported_incremental_sidecar_file(&path)))
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
