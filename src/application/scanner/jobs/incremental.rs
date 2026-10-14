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
            return self
                .finish_empty_incremental_scan_batch(&job, batch_size, cancellation)
                .await;
        }
        let library = self
            .database
            .find_library(&job.library_id)
            .await?
            .ok_or(ScanJobError::LibraryNotFound)?;
        let roots_by_id = self
            .database
            .list_library_roots(&job.library_id)
            .await?
            .into_iter()
            .map(|root| (root.id.clone(), root))
            .collect::<HashMap<_, _>>();
        self.process_incremental_scan_paths(&job, &library.kind, &paths, &roots_by_id, cancellation)
            .await
    }

    async fn finish_empty_incremental_scan_batch(
        &self,
        job: &StoredScanJob,
        batch_size: usize,
        cancellation: &AtomicBool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        self.process_queued_external_subtitle_changes(&job.id, batch_size, cancellation)
            .await?;
        if self
            .cancellation_requested(&job.id, job.cancel_requested, cancellation)
            .await?
        {
            return self.cancel_running_job(&job.id).await;
        }
        if self.database.finish_scan_job_if_idle(&job.id).await? {
            self.database
                .update_library_last_scan(&job.library_id)
                .await?;
            self.record_event(&job.id, "INFO", "JOB_COMPLETED", "局部扫描任务已完成", "{}")
                .await;
            let completed_job = self.database.find_scan_job(&job.id).await?;
            self.publish_webhook_event(
                completed_job.as_ref().unwrap_or(job),
                WebhookEventType::ScanCompleted,
                None,
            )
            .await;
            self.clear_cancellation_flag(&job.id);
            return Ok(ScanBatchReport {
                status: "COMPLETED".to_owned(),
                processed: 0,
                created_items: 0,
                completed: true,
            });
        }
        Ok(ScanBatchReport {
            status: "RUNNING".to_owned(),
            processed: 0,
            created_items: 0,
            completed: false,
        })
    }

    async fn process_incremental_scan_paths(
        &self,
        job: &StoredScanJob,
        library_kind: &str,
        paths: &[StoredScanJobPath],
        roots_by_id: &HashMap<String, StoredLibraryRoot>,
        cancellation: &AtomicBool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let mut created_items = 0_usize;
        for path in paths {
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(&job.id).await;
            }
            let created = match self
                .process_incremental_path(
                    library_kind,
                    job,
                    path,
                    roots_by_id.get(&path.library_root_id),
                    cancellation,
                )
                .await
            {
                Ok(created) => created,
                Err(error) => {
                    self.database
                        .finish_scan_job(&job.id, "FAILED", Some(&error.to_string()))
                        .await?;
                    self.record_event(&job.id, "ERROR", error.code(), "局部扫描任务失败", "{}")
                        .await;
                    let failed_job = self.database.find_scan_job(&job.id).await?;
                    self.publish_webhook_event(
                        failed_job.as_ref().unwrap_or(job),
                        WebhookEventType::ScanFailed,
                        Some(error.code()),
                    )
                    .await;
                    return Err(error.into());
                }
            };
            created_items = created_items.saturating_add(created);
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(&job.id).await;
            }
            self.database
                .mark_scan_job_path_processed(&job.id, &path.library_root_id, &path.relative_path)
                .await?;
        }
        let processed = paths.len();
        let next_count = job
            .processed_count
            .saturating_add(i64::try_from(processed).unwrap_or(i64::MAX));
        self.database
            .update_scan_job_progress(
                &job.id,
                paths.last().map(|path| path.relative_path.as_str()),
                next_count,
            )
            .await?;
        self.record_event(&job.id, "INFO", "BATCH_COMPLETED", "局部扫描批次完成", "{}")
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
            if super::super::library::is_supported_incremental_sidecar_file(&media_path) {
                if super::super::library::is_file_change_only_sidecar(&media_path) {
                    self.database
                        .record_scan_job_file_change(
                            &job.id,
                            &root.id,
                            &path.relative_path,
                            "REMOVE",
                        )
                        .await?;
                } else {
                    let sidecar_paths = [path.relative_path.clone()];
                    let targets_changed = self
                        .database
                        .record_scan_job_sidecar_targets(&job.id, &root.id, &sidecar_paths)
                        .await?;
                    if targets_changed {
                        self.notify_local_metadata_worker(&job.id);
                    }
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
            let mut walker = FileBatchWalker::new_with_sidecars(&media_path);
            while let Some(files) = walker.next_batch(FILE_BATCH_SIZE).await? {
                for file in files {
                    if cancellation.load(Ordering::Acquire) {
                        return Ok(created_items);
                    }
                    if super::super::library::is_supported_incremental_sidecar_file(&file) {
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
        } else if super::super::library::is_supported_incremental_sidecar_file(&media_path) {
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
            if super::super::library::is_file_change_only_sidecar(path) {
                let change_kind = existing_entries
                    .get(&relative_path)
                    .filter(|entry| !entry.is_missing)
                    .map_or("ADD", |_| "CHANGE");
                self.database
                    .record_scan_job_file_change(&job.id, &root.id, &relative_path, change_kind)
                    .await?;
            } else {
                let targets_changed = self
                    .database
                    .record_scan_job_sidecar_targets(&job.id, &root.id, &relative_paths)
                    .await?;
                if targets_changed {
                    self.notify_local_metadata_worker(&job.id);
                }
            }
        }
        Ok(())
    }
}
