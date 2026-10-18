use super::*;

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

    pub(in crate::application::scanner) fn cancellation_flag(
        &self,
        job_id: &str,
    ) -> Arc<AtomicBool> {
        let mut flags = match self.cancellation_flags.lock() {
            Ok(flags) => flags,
            Err(poisoned) => poisoned.into_inner(),
        };
        flags
            .entry(job_id.to_owned())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    }

    pub(in crate::application::scanner) fn clear_cancellation_flag(&self, job_id: &str) {
        let mut flags = match self.cancellation_flags.lock() {
            Ok(flags) => flags,
            Err(poisoned) => poisoned.into_inner(),
        };
        flags.remove(job_id);
    }

    pub(in crate::application::scanner) fn track_scan_job_run(
        &self,
        job_id: &str,
        library_id: &str,
    ) -> Option<ScanJobRunGuard> {
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

    pub(in crate::application::scanner) async fn ensure_lite_manifest_discovery_session(
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

    pub(in crate::application::scanner) fn pop_lite_manifest_directories(
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

    pub(in crate::application::scanner) fn push_lite_manifest_directories(
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

    pub(in crate::application::scanner) fn remove_lite_manifest_root(
        &self,
        manifest_id: &str,
        root_id: &str,
    ) {
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

    pub(in crate::application::scanner) fn clear_lite_manifest_discovery_session(
        &self,
        manifest_id: &str,
    ) {
        let mut sessions = match self.lite_manifest_discovery.lock() {
            Ok(sessions) => sessions,
            Err(poisoned) => poisoned.into_inner(),
        };
        sessions.remove(manifest_id);
    }

    pub(in crate::application::scanner) fn lite_manifest_has_pending_directories(
        &self,
        manifest_id: &str,
    ) -> bool {
        let sessions = match self.lite_manifest_discovery.lock() {
            Ok(sessions) => sessions,
            Err(poisoned) => poisoned.into_inner(),
        };
        sessions
            .get(manifest_id)
            .is_some_and(|session| !session.directories.is_empty())
    }

    pub(in crate::application::scanner) async fn flush_home_after_scan_terminal(&self) {
        if let Some(home) = &self.home {
            if home.flush_scan_invalidation().await {
                self.user_events.publish_home_coalesced().await;
            }
        }
    }

    pub(in crate::application::scanner) fn cancellation_requested_in_memory(
        &self,
        job_id: &str,
    ) -> bool {
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

    pub(in crate::application::scanner) async fn cancel_running_job(
        &self,
        job_id: &str,
    ) -> Result<ScanBatchReport, ScanJobError> {
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

    pub(in crate::application::scanner) async fn cancellation_requested(
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

    pub(in crate::application::scanner) async fn update_activity(
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

    pub(in crate::application::scanner) async fn run_batch_with_failure_handling(
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

    pub(in crate::application::scanner) async fn get_job(
        &self,
        id: &str,
    ) -> Result<ScanJob, ScanJobError> {
        self.database
            .find_scan_job(id)
            .await?
            .map(scan_job)
            .ok_or(ScanJobError::JobNotFound)
    }

    pub(in crate::application::scanner) async fn record_event(
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
