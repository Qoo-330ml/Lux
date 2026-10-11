use super::super::library::{
    MoviePreparationTask, ReconciliationFingerprintTask, ReconciliationRegularGroupTask,
    ReconciliationRegularTask, ReconciliationRegularWork, ReconciliationScanWork,
    collect_reconciliation_fingerprint_task, collect_reconciliation_regular_group_task,
    collect_reconciliation_regular_task, join_movie_preparation, reconciliation_regular_group_key,
};
use super::*;

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
