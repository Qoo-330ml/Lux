use super::*;

mod incremental;
mod lifecycle;
pub(in crate::application::scanner) mod local_metadata;
mod manifest_discovery;
mod manifest_index;
mod postprocessing;
mod reconciliation;

#[derive(Clone)]
pub struct ScanJobService {
    pub(super) scanner: LibraryScanner,
    pub(super) database: Database,
    pub(super) admin_events: AdminEventHub,
    pub(super) user_events: UserEventHub,
    pub(super) scan_lock: Arc<Semaphore>,
    pub(super) full_scan_queue: Arc<Semaphore>,
    pub(super) library_covers: Option<LibraryCoverService>,
    pub(super) strm_probe: Option<StrmProbeService>,
    pub(super) people: Option<PeopleService>,
    pub(super) local_nfo: Option<LocalNfoMetadataStore>,
    pub(super) metadata_selection: Option<MetadataSelectionService>,
    pub(super) metadata_reidentify: Option<MetadataReidentifyService>,
    pub(super) home: Option<HomeService>,
    pub(super) webhooks: Option<WebhookService>,
    pub(super) log_store: LogStore,
    pub(super) resources: ResourceMetrics,
    pub(super) default_scan_concurrency: usize,
    pub(super) scan_concurrency_override: Option<usize>,
    pub(super) cancellation_flags: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    pub(super) scan_run_registry: Arc<Mutex<ScanRunRegistry>>,
    pub(super) metadata_notifications: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    pub(super) local_metadata_image_notifications: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    pub(super) local_metadata_outbox_worker_started: Arc<AtomicBool>,
    pub(super) local_metadata_outbox_notify: Arc<Notify>,
    pub(super) lite_manifest_discovery: Arc<Mutex<HashMap<String, LiteManifestDiscoverySession>>>,
}

#[derive(Default)]
pub(super) struct ActiveScanJobRun {
    pub(super) library_id: String,
    pub(super) run_count: usize,
}

#[derive(Default)]
pub(super) struct ScanRunRegistry {
    pub(super) active_runs: HashMap<String, ActiveScanJobRun>,
    pub(super) deleting_libraries: HashMap<String, usize>,
    pub(super) run_finished: Arc<Notify>,
}

pub(super) struct ScanJobRunGuard {
    pub(super) registry: Arc<Mutex<ScanRunRegistry>>,
    pub(super) job_id: String,
}

impl Drop for ScanJobRunGuard {
    fn drop(&mut self) {
        let (remove_job, run_finished) = {
            let mut registry = match self.registry.lock() {
                Ok(registry) => registry,
                Err(poisoned) => poisoned.into_inner(),
            };
            let remove_job = if let Some(active_run) = registry.active_runs.get_mut(&self.job_id) {
                if active_run.run_count > 1 {
                    active_run.run_count -= 1;
                    false
                } else {
                    true
                }
            } else {
                false
            };
            if remove_job {
                registry.active_runs.remove(&self.job_id);
            }
            (remove_job, Arc::clone(&registry.run_finished))
        };
        if remove_job {
            run_finished.notify_waiters();
        }
    }
}

#[must_use = "keep this guard alive until the library has been deleted"]
pub struct LibraryDeletionGuard {
    pub(super) registry: Arc<Mutex<ScanRunRegistry>>,
    pub(super) library_id: String,
}

impl Drop for LibraryDeletionGuard {
    fn drop(&mut self) {
        let mut registry = match self.registry.lock() {
            Ok(registry) => registry,
            Err(poisoned) => poisoned.into_inner(),
        };
        let remove_library =
            if let Some(deletion_count) = registry.deleting_libraries.get_mut(&self.library_id) {
                if *deletion_count > 1 {
                    *deletion_count -= 1;
                    false
                } else {
                    true
                }
            } else {
                false
            };
        if remove_library {
            registry.deleting_libraries.remove(&self.library_id);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncrementalScanChange {
    pub root_id: String,
    pub relative_path: String,
    pub kind: ChangeKind,
}

/// What a local metadata refresh request queued.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalMetadataRefreshAccepted {
    pub directories: usize,
    pub entries: usize,
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
    LibraryDeletionScanTimeout,
    Scanner(ScannerError),
    Storage(StorageError),
}
