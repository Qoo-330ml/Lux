use super::*;

impl ScanJobService {
    pub(in crate::application::scanner) async fn run_incremental_batch(
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
}
