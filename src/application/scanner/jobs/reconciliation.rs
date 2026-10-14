use super::super::library::{
    MoviePreparationTask, ReconciliationFingerprintTask, ReconciliationRegularGroupTask,
    ReconciliationRegularTask, ReconciliationRegularWork, ReconciliationScanWork,
    collect_reconciliation_fingerprint_task, collect_reconciliation_regular_group_task,
    collect_reconciliation_regular_task, join_movie_preparation, reconciliation_regular_group_key,
};
use super::*;

#[derive(Default)]
struct ReconciliationFileBatchState {
    processed: usize,
    next_count: i64,
    created_items: usize,
    completed_entries: Vec<StoredReconciliationScanEntry>,
    existing_entries_by_root: HashMap<String, HashMap<String, StoredFilesystemEntry>>,
    changed_paths_by_root: HashMap<String, Vec<String>>,
    new_paths_by_root: HashMap<String, Vec<String>>,
    changed_sidecar_paths_by_root: HashMap<String, Vec<String>>,
    file_change_events_by_root: HashMap<String, Vec<(String, String)>>,
    new_works_by_root: HashMap<String, Vec<ReconciliationScanWork>>,
    root_has_existing_index: HashMap<String, bool>,
    quick_seen_entry_ids: HashMap<String, Vec<String>>,
    missing_paths_by_root: HashMap<String, Vec<String>>,
    regular_works: Vec<ReconciliationRegularWork>,
    movie_files_by_root: HashMap<String, Vec<NewMovieFile>>,
    episode_files_by_root: HashMap<String, Vec<NewEpisodeFile>>,
}

struct ReconciliationFileBatchContext<'a> {
    job: &'a StoredScanJob,
    library_kind: &'a str,
    roots: &'a [StoredLibraryRoot],
    batch: &'a [StoredReconciliationScanEntry],
    cancellation: &'a AtomicBool,
}

impl ReconciliationFileBatchState {
    fn new(next_count: i64) -> Self {
        Self {
            next_count,
            ..Self::default()
        }
    }
}

#[derive(Default)]
struct MovieReconciliationFileBatchState {
    processed: usize,
    next_count: i64,
    created_items: usize,
    completed_entries: Vec<(usize, StoredReconciliationScanEntry)>,
    unavailable_root_ids: HashSet<String>,
    existing_entries_by_root: HashMap<String, HashMap<String, StoredFilesystemEntry>>,
    changed_paths_by_root: HashMap<String, Vec<String>>,
    new_paths_by_root: HashMap<String, Vec<String>>,
    changed_sidecar_paths_by_root: HashMap<String, Vec<String>>,
    file_change_events_by_root: HashMap<String, Vec<(String, String)>>,
    quick_seen_entry_ids: HashMap<String, Vec<String>>,
    missing_paths_by_root: HashMap<String, Vec<String>>,
    new_files: Vec<(
        usize,
        String,
        PathBuf,
        PathBuf,
        StoredReconciliationScanEntry,
    )>,
    regular_works: Vec<ReconciliationRegularWork>,
    prepared_files: HashMap<String, Vec<NewMovieFile>>,
}

struct MovieReconciliationFileBatchContext<'a> {
    job: &'a StoredScanJob,
    roots: &'a [StoredLibraryRoot],
    batch: &'a [StoredReconciliationScanEntry],
    cancellation: &'a AtomicBool,
}

impl MovieReconciliationFileBatchState {
    fn new(next_count: i64) -> Self {
        Self {
            next_count,
            ..Self::default()
        }
    }

    fn completed_entry_snapshot(&self) -> Vec<StoredReconciliationScanEntry> {
        self.completed_entries
            .iter()
            .map(|(_, entry)| entry.clone())
            .collect()
    }
}

impl ScanJobService {
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
                    && (is_supported_movie_file(&path)
                        || super::super::library::is_supported_incremental_sidecar_file(&path)))
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

    pub(in crate::application::scanner) async fn run_reconciliation_discovery_batch(
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

    pub(in crate::application::scanner) async fn run_reconciliation_file_batch(
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
        let configured_concurrency = configured_scan_concurrency(
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
            return self.finish_reconciliation_file_batch(job, &roots).await;
        }
        if library_kind == "MOVIE" {
            return self
                .run_movie_reconciliation_file_batch(
                    job,
                    &roots,
                    &batch,
                    configured_concurrency,
                    cancellation,
                )
                .await;
        }
        self.run_non_movie_reconciliation_file_batch(
            job,
            library_kind,
            &roots,
            &batch,
            configured_concurrency,
            cancellation,
        )
        .await
    }

    async fn run_non_movie_reconciliation_file_batch(
        &self,
        job: &StoredScanJob,
        library_kind: &str,
        roots: &[StoredLibraryRoot],
        batch: &[StoredReconciliationScanEntry],
        configured_concurrency: i64,
        cancellation: &AtomicBool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let mut state = ReconciliationFileBatchState::new(job.processed_count);
        state.existing_entries_by_root = self
            .load_reconciliation_batch_existing_entries(roots, batch)
            .await?;
        let concurrency = self
            .effective_scan_concurrency(configured_concurrency)
            .await;
        let mut quick_results = self
            .scan_reconciliation_file_fingerprints(
                roots,
                batch,
                &state.existing_entries_by_root,
                concurrency,
            )
            .await?;
        let context = ReconciliationFileBatchContext {
            job,
            library_kind,
            roots,
            batch,
            cancellation,
        };
        let mut classification_cache = MixedClassificationCache::default();
        for index in 0..batch.len() {
            if let Some(report) = self
                .process_reconciliation_file_entry(
                    &context,
                    &mut quick_results,
                    &mut state,
                    &mut classification_cache,
                    index,
                )
                .await?
            {
                return Ok(report);
            }
        }
        let regular_works = std::mem::take(&mut state.regular_works);
        if let Some(report) = self
            .scan_reconciliation_regular_works(
                job,
                &regular_works,
                &mut state,
                concurrency,
                library_kind,
                cancellation,
            )
            .await?
        {
            return Ok(report);
        }
        if let Some(report) = self
            .prepare_non_movie_reconciliation_files(
                job,
                roots,
                &mut state,
                concurrency,
                cancellation,
            )
            .await?
        {
            return Ok(report);
        }
        if cancellation.load(Ordering::Acquire) {
            return self.cancel_running_job(&job.id).await;
        }
        self.commit_non_movie_reconciliation_batch(job, roots, state, concurrency)
            .await
    }

    async fn load_reconciliation_batch_existing_entries(
        &self,
        roots: &[StoredLibraryRoot],
        batch: &[StoredReconciliationScanEntry],
    ) -> Result<HashMap<String, HashMap<String, StoredFilesystemEntry>>, ScanJobError> {
        let mut paths_by_root = HashMap::<String, Vec<String>>::new();
        for entry in batch {
            if roots.iter().any(|root| root.id == entry.library_root_id) {
                paths_by_root
                    .entry(entry.library_root_id.clone())
                    .or_default()
                    .push(entry.relative_path.clone());
            }
        }
        let mut existing_by_root = HashMap::new();
        for (root_id, paths) in paths_by_root {
            let entries = self
                .database
                .list_filesystem_entries_for_paths(&root_id, &paths)
                .await?;
            existing_by_root.insert(root_id, entries);
        }
        Ok(existing_by_root)
    }

    async fn process_reconciliation_file_entry(
        &self,
        context: &ReconciliationFileBatchContext<'_>,
        quick_results: &mut [Option<(String, ScanReport)>],
        state: &mut ReconciliationFileBatchState,
        classification_cache: &mut MixedClassificationCache,
        entry_index: usize,
    ) -> Result<Option<ScanBatchReport>, ScanJobError> {
        let job = context.job;
        let roots = context.roots;
        let batch = context.batch;
        let cancellation = context.cancellation;
        if cancellation.load(Ordering::Acquire) {
            return self.cancel_running_job(&job.id).await.map(Some);
        }
        let entry = &batch[entry_index];
        let Some(root) = roots.iter().find(|root| root.id == entry.library_root_id) else {
            self.database
                .discard_reconciliation_root_entries(&job.id, &entry.library_root_id)
                .await?;
            return Ok(None);
        };
        if !root.is_available {
            self.discard_unavailable_reconciliation_root(job, root, state, 0)
                .await?;
            return Ok(None);
        }
        let path = Path::new(&root.canonical_path).join(&entry.relative_path);
        if fs::metadata(&path).await.is_err() {
            let root_is_available = fs::metadata(&root.canonical_path)
                .await
                .is_ok_and(|metadata| metadata.is_dir());
            if !root_is_available {
                let already_processed = state
                    .completed_entries
                    .iter()
                    .filter(|completed| completed.library_root_id == root.id)
                    .count();
                self.discard_unavailable_reconciliation_root(job, root, state, already_processed)
                    .await?;
            } else {
                state.next_count = state.next_count.saturating_add(1);
                state.processed = state.processed.saturating_add(1);
                state
                    .missing_paths_by_root
                    .entry(root.id.clone())
                    .or_default()
                    .push(entry.relative_path.clone());
                state.completed_entries.push(entry.clone());
            }
            return Ok(None);
        }
        if !is_supported_movie_file(&path) {
            self.process_reconciliation_sidecar(job, root, entry, &path, state)
                .await?;
            return Ok(None);
        }
        if self.process_reconciliation_quick_result(
            job,
            root,
            entry,
            quick_results[entry_index].take(),
            state,
        ) {
            return Ok(None);
        }
        self.queue_reconciliation_media_work(
            context,
            root,
            entry,
            &path,
            state,
            classification_cache,
        )
        .await?;
        Ok(None)
    }

    async fn discard_unavailable_reconciliation_root(
        &self,
        job: &StoredScanJob,
        root: &StoredLibraryRoot,
        state: &mut ReconciliationFileBatchState,
        already_processed: usize,
    ) -> Result<(), ScanJobError> {
        state.regular_works.retain(|work| work.root.id != root.id);
        state.new_works_by_root.remove(&root.id);
        self.database
            .update_library_root_availability(&root.id, false)
            .await?;
        let discarded = self
            .database
            .discard_reconciliation_root_entries(&job.id, &root.id)
            .await?
            .saturating_sub(i64::try_from(already_processed).unwrap_or(i64::MAX));
        state.next_count = state.next_count.saturating_add(discarded);
        state.processed = state
            .processed
            .saturating_add(usize::try_from(discarded).unwrap_or(usize::MAX));
        self.database
            .update_scan_job_progress(&job.id, None, state.next_count)
            .await?;
        Ok(())
    }

    async fn process_reconciliation_sidecar(
        &self,
        job: &StoredScanJob,
        root: &StoredLibraryRoot,
        entry: &StoredReconciliationScanEntry,
        path: &Path,
        state: &mut ReconciliationFileBatchState,
    ) -> Result<(), ScanJobError> {
        let existing_entries = state
            .existing_entries_by_root
            .get(&root.id)
            .ok_or(ScanJobError::LibraryNotFound)?;
        let (entry_id, changed) = self
            .scanner
            .scan_sidecar_file(
                root,
                Path::new(&root.canonical_path),
                path,
                existing_entries,
                &job.generation,
            )
            .await?;
        let target_needs_recovery = existing_entries
            .get(&entry.relative_path)
            .is_some_and(|existing| existing.last_seen_generation == job.generation);
        if changed || target_needs_recovery {
            if super::super::library::is_file_change_only_sidecar(path) {
                let change_kind = if target_needs_recovery
                    || existing_entries
                        .get(&entry.relative_path)
                        .is_some_and(|existing| !existing.is_missing)
                {
                    "CHANGE"
                } else {
                    "ADD"
                };
                state
                    .file_change_events_by_root
                    .entry(root.id.clone())
                    .or_default()
                    .push((entry.relative_path.clone(), change_kind.to_owned()));
            } else {
                state
                    .changed_sidecar_paths_by_root
                    .entry(root.id.clone())
                    .or_default()
                    .push(entry.relative_path.clone());
            }
        } else {
            state
                .quick_seen_entry_ids
                .entry(root.id.clone())
                .or_default()
                .push(entry_id);
        }
        state.next_count = state.next_count.saturating_add(1);
        state.processed = state.processed.saturating_add(1);
        state.completed_entries.push(entry.clone());
        Ok(())
    }

    fn process_reconciliation_quick_result(
        &self,
        job: &StoredScanJob,
        root: &StoredLibraryRoot,
        entry: &StoredReconciliationScanEntry,
        quick_result: Option<(String, ScanReport)>,
        state: &mut ReconciliationFileBatchState,
    ) -> bool {
        let Some((entry_id, quick_report)) = quick_result else {
            return false;
        };
        state
            .quick_seen_entry_ids
            .entry(root.id.clone())
            .or_default()
            .push(entry_id);
        if state
            .existing_entries_by_root
            .get(&root.id)
            .and_then(|entries| entries.get(&entry.relative_path))
            .is_some_and(|existing| existing.last_seen_generation == job.generation)
        {
            state
                .changed_paths_by_root
                .entry(root.id.clone())
                .or_default()
                .push(entry.relative_path.clone());
        }
        state.next_count = state.next_count.saturating_add(1);
        state.processed = state.processed.saturating_add(1);
        state.created_items = state
            .created_items
            .saturating_add(quick_report.created_items);
        state.completed_entries.push(entry.clone());
        true
    }

    async fn queue_reconciliation_media_work(
        &self,
        context: &ReconciliationFileBatchContext<'_>,
        root: &StoredLibraryRoot,
        entry: &StoredReconciliationScanEntry,
        path: &Path,
        state: &mut ReconciliationFileBatchState,
        classification_cache: &mut MixedClassificationCache,
    ) -> Result<(), ScanJobError> {
        let job = context.job;
        let library_kind = context.library_kind;
        let existing_entries = state
            .existing_entries_by_root
            .get(&root.id)
            .ok_or(ScanJobError::LibraryNotFound)?;
        let classification = match library_kind {
            "SERIES" => MixedClassification::Episode,
            "HOMEVIDEOS" => MixedClassification::Unresolved,
            "MIXED" => {
                classify_mixed_file(Path::new(&root.canonical_path), path, classification_cache)
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
            if let Some(has_index) = state.root_has_existing_index.get(&root.id) {
                *has_index
            } else {
                let has_index = self
                    .database
                    .has_filesystem_entries_for_root(&root.id)
                    .await?;
                state
                    .root_has_existing_index
                    .insert(root.id.clone(), has_index);
                has_index
            }
        } else {
            false
        };
        let requires_compatibility_scan = batchable
            && root_has_index
            && (self
                .scanner
                .file_has_moved_entry(&job.library_id, root, Path::new(&root.canonical_path), path)
                .await?
                || (matches!(classification, MixedClassification::Episode)
                    && self
                        .scanner
                        .episode_path_has_legacy_identity(
                            root,
                            Path::new(&root.canonical_path),
                            path,
                        )
                        .await?));
        if batchable && !requires_compatibility_scan {
            state
                .new_paths_by_root
                .entry(root.id.clone())
                .or_default()
                .push(entry.relative_path.clone());
            state
                .new_works_by_root
                .entry(root.id.clone())
                .or_default()
                .push(ReconciliationScanWork {
                    entry: entry.clone(),
                    path: path.to_owned(),
                    classification,
                });
            return Ok(());
        }
        let target_paths = if existing_entries.contains_key(&entry.relative_path) {
            &mut state.changed_paths_by_root
        } else {
            &mut state.new_paths_by_root
        };
        target_paths
            .entry(root.id.clone())
            .or_default()
            .push(entry.relative_path.clone());
        state.regular_works.push(ReconciliationRegularWork {
            index: state.regular_works.len(),
            entry: entry.clone(),
            root: root.clone(),
            path: path.to_owned(),
            classification,
        });
        Ok(())
    }

    async fn scan_reconciliation_regular_works(
        &self,
        job: &StoredScanJob,
        regular_works: &[ReconciliationRegularWork],
        state: &mut ReconciliationFileBatchState,
        concurrency: usize,
        library_kind: &str,
        cancellation: &AtomicBool,
    ) -> Result<Option<ScanBatchReport>, ScanJobError> {
        let mut tasks: JoinSet<ReconciliationRegularTask> = JoinSet::new();
        let mut results = (0..regular_works.len()).map(|_| None).collect::<Vec<_>>();
        for (index, work) in regular_works.iter().enumerate() {
            if cancellation.load(Ordering::Acquire) {
                tasks.abort_all();
                return self.cancel_running_job(&job.id).await.map(Some);
            }
            while tasks.len() >= concurrency {
                if let Err(error) =
                    collect_reconciliation_regular_task(&mut tasks, &mut results).await
                {
                    return self
                        .fail_reconciliation_job(
                            job,
                            error,
                            &state.completed_entries,
                            state.next_count,
                        )
                        .await
                        .map(Some);
                }
            }
            let scanner = self.scanner.clone();
            let library_id = job.library_id.clone();
            let generation = job.generation.clone();
            let root = work.root.clone();
            let path = work.path.clone();
            let classification = work.classification;
            let is_home_videos = library_kind == "HOMEVIDEOS";
            tasks.spawn(async move {
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
        while !tasks.is_empty() {
            if let Err(error) = collect_reconciliation_regular_task(&mut tasks, &mut results).await
            {
                return self
                    .fail_reconciliation_job(job, error, &state.completed_entries, state.next_count)
                    .await
                    .map(Some);
            }
        }
        if cancellation.load(Ordering::Acquire) {
            return self.cancel_running_job(&job.id).await.map(Some);
        }
        for (work, report) in regular_works.iter().zip(results.into_iter().flatten()) {
            state.created_items = state.created_items.saturating_add(report.created_items);
            state.next_count = state.next_count.saturating_add(1);
            state.processed = state.processed.saturating_add(1);
            state.completed_entries.push(work.entry.clone());
        }
        Ok(None)
    }

    async fn prepare_non_movie_reconciliation_files(
        &self,
        job: &StoredScanJob,
        roots: &[StoredLibraryRoot],
        state: &mut ReconciliationFileBatchState,
        concurrency: usize,
        cancellation: &AtomicBool,
    ) -> Result<Option<ScanBatchReport>, ScanJobError> {
        for (root_id, works) in std::mem::take(&mut state.new_works_by_root) {
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(&job.id).await.map(Some);
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
                            .fail_reconciliation_job(
                                job,
                                error,
                                &state.completed_entries,
                                state.next_count,
                            )
                            .await
                            .map(Some);
                    }
                };
                for (work, prepared) in movie_works.into_iter().zip(prepared) {
                    if let Some(file) = prepared {
                        state
                            .movie_files_by_root
                            .entry(root_id.clone())
                            .or_default()
                            .push(file);
                    }
                    state.next_count = state.next_count.saturating_add(1);
                    state.processed = state.processed.saturating_add(1);
                    state.completed_entries.push(work.entry);
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
                            .fail_reconciliation_job(
                                job,
                                error,
                                &state.completed_entries,
                                state.next_count,
                            )
                            .await
                            .map(Some);
                    }
                };
                for (work, prepared) in episode_works.into_iter().zip(prepared) {
                    if let Some(file) = prepared {
                        state
                            .episode_files_by_root
                            .entry(root_id.clone())
                            .or_default()
                            .push(file);
                    }
                    state.next_count = state.next_count.saturating_add(1);
                    state.processed = state.processed.saturating_add(1);
                    state.completed_entries.push(work.entry);
                }
            }
        }
        Ok(None)
    }

    async fn commit_non_movie_reconciliation_batch(
        &self,
        job: &StoredScanJob,
        roots: &[StoredLibraryRoot],
        mut state: ReconciliationFileBatchState,
        concurrency: usize,
    ) -> Result<ScanBatchReport, ScanJobError> {
        for root in roots {
            let root_entries = state
                .completed_entries
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
                movie_files: state
                    .movie_files_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                episode_files: state
                    .episode_files_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                seen_entry_ids: state
                    .quick_seen_entry_ids
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                missing_paths: state
                    .missing_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                new_paths: state
                    .new_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                changed_paths: state
                    .changed_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                sidecar_paths: state
                    .changed_sidecar_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                file_change_events: state
                    .file_change_events_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
            };
            let (confirmed, inserted) = self.commit_reconciliation_root_batch(&batch).await?;
            // Keep the root-level confirmation count authoritative when the storage batch skips stale entries.
            state.processed = state
                .processed
                .saturating_sub(root_entries.len())
                .saturating_add(confirmed);
            state.created_items = state.created_items.saturating_add(inserted);
        }
        self.record_reconciliation_batch_event(
            job,
            state.processed,
            state.created_items,
            state.next_count,
            concurrency,
        )
        .await
    }

    async fn finish_reconciliation_file_batch(
        &self,
        job: &StoredScanJob,
        roots: &[StoredLibraryRoot],
    ) -> Result<ScanBatchReport, ScanJobError> {
        let mut removed_count = 0_usize;
        for root in roots {
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
                let removed_file_change_paths = confirmed_missing_paths
                    .iter()
                    .filter(|path| {
                        super::super::library::is_file_change_only_sidecar(Path::new(path))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let removed_sidecar_paths = confirmed_missing_paths
                    .iter()
                    .filter(|path| {
                        super::super::library::is_supported_incremental_sidecar_file(Path::new(
                            path,
                        )) && !super::super::library::is_file_change_only_sidecar(Path::new(path))
                    })
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
                                    (
                                        &removed_media_paths,
                                        &removed_sidecar_paths,
                                        &removed_file_change_paths,
                                    ),
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
        Ok(ScanBatchReport {
            status: "COMPLETED".to_owned(),
            processed: 0,
            created_items: 0,
            completed: true,
        })
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
        let mut state = MovieReconciliationFileBatchState::new(job.processed_count);
        state.existing_entries_by_root = self
            .load_reconciliation_batch_existing_entries(roots, batch)
            .await?;
        let concurrency = self
            .effective_scan_concurrency(configured_concurrency)
            .await;
        let mut quick_results = self
            .scan_reconciliation_file_fingerprints(
                roots,
                batch,
                &state.existing_entries_by_root,
                concurrency,
            )
            .await?;
        let context = MovieReconciliationFileBatchContext {
            job,
            roots,
            batch,
            cancellation,
        };
        for index in 0..batch.len() {
            if let Some(report) = self
                .process_movie_reconciliation_entry(&context, &mut quick_results, &mut state, index)
                .await?
            {
                return Ok(report);
            }
        }
        if let Some(report) = self
            .scan_movie_reconciliation_regular_groups(job, &mut state, concurrency, cancellation)
            .await?
        {
            return Ok(report);
        }
        if let Some(report) = self
            .prepare_movie_reconciliation_new_files(job, &mut state, concurrency, cancellation)
            .await?
        {
            return Ok(report);
        }
        if cancellation.load(Ordering::Acquire) {
            return self.cancel_running_job(&job.id).await;
        }
        self.commit_movie_reconciliation_batch(job, roots, state, concurrency)
            .await
    }

    async fn process_movie_reconciliation_entry(
        &self,
        context: &MovieReconciliationFileBatchContext<'_>,
        quick_results: &mut [Option<(String, ScanReport)>],
        state: &mut MovieReconciliationFileBatchState,
        index: usize,
    ) -> Result<Option<ScanBatchReport>, ScanJobError> {
        let job = context.job;
        let roots = context.roots;
        let batch = context.batch;
        let cancellation = context.cancellation;
        if cancellation.load(Ordering::Acquire) {
            return self.cancel_running_job(&job.id).await.map(Some);
        }
        let entry = &batch[index];
        if state.unavailable_root_ids.contains(&entry.library_root_id) {
            return Ok(None);
        }
        let Some(root) = roots.iter().find(|root| root.id == entry.library_root_id) else {
            self.database
                .discard_reconciliation_root_entries(&job.id, &entry.library_root_id)
                .await?;
            state
                .unavailable_root_ids
                .insert(entry.library_root_id.clone());
            return Ok(None);
        };
        if !root.is_available {
            state.unavailable_root_ids.insert(root.id.clone());
            self.discard_unavailable_movie_reconciliation_root(job, root, state, 0)
                .await?;
            return Ok(None);
        }
        let path = Path::new(&root.canonical_path).join(&entry.relative_path);
        if fs::metadata(&path).await.is_err() {
            let root_is_available = fs::metadata(&root.canonical_path)
                .await
                .is_ok_and(|metadata| metadata.is_dir());
            if !root_is_available {
                state.unavailable_root_ids.insert(root.id.clone());
                let already_processed = state
                    .completed_entries
                    .iter()
                    .filter(|(_, completed)| completed.library_root_id == root.id)
                    .count();
                self.discard_unavailable_movie_reconciliation_root(
                    job,
                    root,
                    state,
                    already_processed,
                )
                .await?;
            } else {
                state.next_count = state.next_count.saturating_add(1);
                state.processed = state.processed.saturating_add(1);
                state
                    .missing_paths_by_root
                    .entry(root.id.clone())
                    .or_default()
                    .push(entry.relative_path.clone());
                state.completed_entries.push((index, entry.clone()));
            }
            return Ok(None);
        }
        if !is_supported_movie_file(&path) {
            self.process_movie_reconciliation_sidecar(job, root, entry, &path, index, state)
                .await?;
            return Ok(None);
        }
        let existing_entries = state
            .existing_entries_by_root
            .get(&root.id)
            .ok_or(ScanJobError::LibraryNotFound)?;
        if let Some((entry_id, quick_report)) = quick_results[index].take() {
            state
                .quick_seen_entry_ids
                .entry(root.id.clone())
                .or_default()
                .push(entry_id);
            if existing_entries
                .get(&entry.relative_path)
                .is_some_and(|existing| existing.last_seen_generation == job.generation)
            {
                state
                    .changed_paths_by_root
                    .entry(root.id.clone())
                    .or_default()
                    .push(entry.relative_path.clone());
            }
            state.next_count = state.next_count.saturating_add(1);
            state.processed = state.processed.saturating_add(1);
            state.completed_entries.push((index, entry.clone()));
            state.created_items = state
                .created_items
                .saturating_add(quick_report.created_items);
            return Ok(None);
        }
        if existing_entries.contains_key(&entry.relative_path) {
            state
                .changed_paths_by_root
                .entry(root.id.clone())
                .or_default()
                .push(entry.relative_path.clone());
            state.regular_works.push(ReconciliationRegularWork {
                index,
                entry: entry.clone(),
                root: root.clone(),
                path,
                classification: MixedClassification::Movie,
            });
            return Ok(None);
        }
        state
            .new_paths_by_root
            .entry(root.id.clone())
            .or_default()
            .push(entry.relative_path.clone());
        state.new_files.push((
            index,
            root.id.clone(),
            PathBuf::from(&root.canonical_path),
            path,
            entry.clone(),
        ));
        Ok(None)
    }

    async fn discard_unavailable_movie_reconciliation_root(
        &self,
        job: &StoredScanJob,
        root: &StoredLibraryRoot,
        state: &mut MovieReconciliationFileBatchState,
        already_processed: usize,
    ) -> Result<(), ScanJobError> {
        self.database
            .update_library_root_availability(&root.id, false)
            .await?;
        let discarded = self
            .database
            .discard_reconciliation_root_entries(&job.id, &root.id)
            .await?
            .saturating_sub(i64::try_from(already_processed).unwrap_or(i64::MAX));
        state.next_count = state.next_count.saturating_add(discarded);
        state.processed = state
            .processed
            .saturating_add(usize::try_from(discarded).unwrap_or(usize::MAX));
        self.database
            .update_scan_job_progress(&job.id, None, state.next_count)
            .await?;
        Ok(())
    }

    async fn process_movie_reconciliation_sidecar(
        &self,
        job: &StoredScanJob,
        root: &StoredLibraryRoot,
        entry: &StoredReconciliationScanEntry,
        path: &Path,
        index: usize,
        state: &mut MovieReconciliationFileBatchState,
    ) -> Result<(), ScanJobError> {
        let existing_entries = state
            .existing_entries_by_root
            .get(&root.id)
            .ok_or(ScanJobError::LibraryNotFound)?;
        let (entry_id, changed) = self
            .scanner
            .scan_sidecar_file(
                root,
                Path::new(&root.canonical_path),
                path,
                existing_entries,
                &job.generation,
            )
            .await?;
        let target_needs_recovery = existing_entries
            .get(&entry.relative_path)
            .is_some_and(|existing| existing.last_seen_generation == job.generation);
        if changed || target_needs_recovery {
            if super::super::library::is_file_change_only_sidecar(path) {
                let change_kind = if target_needs_recovery
                    || existing_entries
                        .get(&entry.relative_path)
                        .is_some_and(|existing| !existing.is_missing)
                {
                    "CHANGE"
                } else {
                    "ADD"
                };
                state
                    .file_change_events_by_root
                    .entry(root.id.clone())
                    .or_default()
                    .push((entry.relative_path.clone(), change_kind.to_owned()));
            } else {
                state
                    .changed_sidecar_paths_by_root
                    .entry(root.id.clone())
                    .or_default()
                    .push(entry.relative_path.clone());
            }
        } else {
            state
                .quick_seen_entry_ids
                .entry(root.id.clone())
                .or_default()
                .push(entry_id);
        }
        state.next_count = state.next_count.saturating_add(1);
        state.processed = state.processed.saturating_add(1);
        state.completed_entries.push((index, entry.clone()));
        Ok(())
    }

    async fn scan_movie_reconciliation_regular_groups(
        &self,
        job: &StoredScanJob,
        state: &mut MovieReconciliationFileBatchState,
        concurrency: usize,
        cancellation: &AtomicBool,
    ) -> Result<Option<ScanBatchReport>, ScanJobError> {
        let mut grouped_works =
            Vec::<Vec<(usize, ReconciliationRegularWork, Option<String>)>>::new();
        let mut group_indexes = HashMap::<String, usize>::new();
        for (regular_index, work) in state.regular_works.iter().cloned().enumerate() {
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
            let group_index = *group_indexes.entry(group_key).or_insert_with(|| {
                grouped_works.push(Vec::new());
                grouped_works.len() - 1
            });
            grouped_works[group_index].push((regular_index, work, inferred_suffix));
        }
        let mut tasks: JoinSet<ReconciliationRegularGroupTask> = JoinSet::new();
        let mut results = (0..state.regular_works.len())
            .map(|_| None)
            .collect::<Vec<_>>();
        for group in grouped_works {
            if cancellation.load(Ordering::Acquire) {
                tasks.abort_all();
                return self.cancel_running_job(&job.id).await.map(Some);
            }
            while tasks.len() >= concurrency {
                if let Err(error) =
                    collect_reconciliation_regular_group_task(&mut tasks, &mut results).await
                {
                    return self
                        .fail_reconciliation_job(
                            job,
                            error,
                            &state.completed_entry_snapshot(),
                            state.next_count,
                        )
                        .await
                        .map(Some);
                }
            }
            let scanner = self.scanner.clone();
            let library_id = job.library_id.clone();
            let generation = job.generation.clone();
            tasks.spawn(async move {
                let mut reports = Vec::with_capacity(group.len());
                for (regular_index, work, inferred_suffix) in group {
                    let root = work.root;
                    let path = work.path;
                    let report = scanner
                        .scan_movie_file_with_inferred_suffix(
                            &library_id,
                            &root,
                            Path::new(&root.canonical_path),
                            &path,
                            &generation,
                            inferred_suffix.as_deref(),
                        )
                        .await?;
                    reports.push((regular_index, report));
                }
                Ok(reports)
            });
        }
        while !tasks.is_empty() {
            if let Err(error) =
                collect_reconciliation_regular_group_task(&mut tasks, &mut results).await
            {
                return self
                    .fail_reconciliation_job(
                        job,
                        error,
                        &state.completed_entry_snapshot(),
                        state.next_count,
                    )
                    .await
                    .map(Some);
            }
        }
        if cancellation.load(Ordering::Acquire) {
            return self.cancel_running_job(&job.id).await.map(Some);
        }
        for (work, report) in state
            .regular_works
            .iter()
            .zip(results.into_iter().flatten())
        {
            state.created_items = state.created_items.saturating_add(report.created_items);
            state.next_count = state.next_count.saturating_add(1);
            state.processed = state.processed.saturating_add(1);
            state
                .completed_entries
                .push((work.index, work.entry.clone()));
        }
        Ok(None)
    }

    async fn prepare_movie_reconciliation_new_files(
        &self,
        job: &StoredScanJob,
        state: &mut MovieReconciliationFileBatchState,
        concurrency: usize,
        cancellation: &AtomicBool,
    ) -> Result<Option<ScanBatchReport>, ScanJobError> {
        let mut tasks: JoinSet<MoviePreparationTask> = JoinSet::new();
        let mut active_tasks = 0_usize;
        for (index, root_id, root_path, path, entry) in std::mem::take(&mut state.new_files) {
            if cancellation.load(Ordering::Acquire) {
                tasks.abort_all();
                return self.cancel_running_job(&job.id).await.map(Some);
            }
            if active_tasks >= concurrency {
                if let Some(report) = self
                    .collect_movie_reconciliation_preparation(
                        job,
                        state,
                        &mut tasks,
                        &mut active_tasks,
                        cancellation,
                    )
                    .await?
                {
                    return Ok(Some(report));
                }
            }
            let scanner = self.scanner.clone();
            tasks.spawn(async move {
                let prepared = scanner.prepare_new_movie_file(&root_path, &path).await?;
                Ok((index, root_id, entry, prepared))
            });
            active_tasks = active_tasks.saturating_add(1);
        }
        while active_tasks > 0 {
            if let Some(report) = self
                .collect_movie_reconciliation_preparation(
                    job,
                    state,
                    &mut tasks,
                    &mut active_tasks,
                    cancellation,
                )
                .await?
            {
                return Ok(Some(report));
            }
        }
        Ok(None)
    }

    async fn collect_movie_reconciliation_preparation(
        &self,
        job: &StoredScanJob,
        state: &mut MovieReconciliationFileBatchState,
        tasks: &mut JoinSet<MoviePreparationTask>,
        active_tasks: &mut usize,
        cancellation: &AtomicBool,
    ) -> Result<Option<ScanBatchReport>, ScanJobError> {
        let (index, root_id, entry, file) = match join_movie_preparation(tasks).await {
            Ok(result) => result,
            Err(error) => {
                return self
                    .fail_reconciliation_job(
                        job,
                        error,
                        &state.completed_entry_snapshot(),
                        state.next_count,
                    )
                    .await
                    .map(Some);
            }
        };
        *active_tasks = active_tasks.saturating_sub(1);
        if cancellation.load(Ordering::Acquire) {
            tasks.abort_all();
            return self.cancel_running_job(&job.id).await.map(Some);
        }
        if let Some(file) = file {
            state.prepared_files.entry(root_id).or_default().push(file);
        }
        state.next_count = state.next_count.saturating_add(1);
        state.processed = state.processed.saturating_add(1);
        state.completed_entries.push((index, entry));
        Ok(None)
    }

    async fn commit_movie_reconciliation_batch(
        &self,
        job: &StoredScanJob,
        roots: &[StoredLibraryRoot],
        mut state: MovieReconciliationFileBatchState,
        concurrency: usize,
    ) -> Result<ScanBatchReport, ScanJobError> {
        state.completed_entries.sort_by_key(|(index, _)| *index);
        let completed_entries = state
            .completed_entries
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
                movie_files: state
                    .prepared_files
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                episode_files: &[],
                seen_entry_ids: state
                    .quick_seen_entry_ids
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                missing_paths: state
                    .missing_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                new_paths: state
                    .new_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                changed_paths: state
                    .changed_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                sidecar_paths: state
                    .changed_sidecar_paths_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
                file_change_events: state
                    .file_change_events_by_root
                    .get(&root.id)
                    .map_or(&[][..], Vec::as_slice),
            };
            let (confirmed, inserted) = self.commit_reconciliation_root_batch(&batch).await?;
            state.processed = state
                .processed
                .saturating_sub(root_entries.len())
                .saturating_add(confirmed);
            state.created_items = state.created_items.saturating_add(inserted);
        }
        self.record_reconciliation_batch_event(
            job,
            state.processed,
            state.created_items,
            state.next_count,
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

    pub(in crate::application::scanner) async fn fail_reconciliation_job(
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
}
