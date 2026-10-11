use super::*;

impl ScanJobService {
    pub(in crate::application::scanner) async fn run_scan_manifest_diff_batch(
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

    pub(in crate::application::scanner) async fn run_scan_manifest_apply_batch(
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

    pub(in crate::application::scanner) async fn materialize_manifest_postprocessing_targets_unlocked(
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

    pub(in crate::application::scanner) async fn finish_scan_manifest_indexing(
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
}
