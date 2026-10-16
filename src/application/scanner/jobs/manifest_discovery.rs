use super::*;
use crate::application::background_work::{BackgroundWorkStage, acquire_process_background_work};

impl ScanJobService {
    pub(in crate::application::scanner) async fn discover_scan_manifest_directory_batches(
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
            ..
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
        let open_permit = acquire_process_background_work(BackgroundWorkStage::Discovery)
            .await
            .map_err(|source| ScannerError::Io {
                path: root_path.clone(),
                source: std::io::Error::other(source.to_string()),
            })?;
        if cancellation.load(Ordering::Acquire) {
            return Ok(None);
        }
        let mut reader = tokio::task::spawn_blocking(move || {
            let _permit = open_permit;
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
            let read_permit = acquire_process_background_work(BackgroundWorkStage::Discovery)
                .await
                .map_err(|source| ScannerError::Io {
                    path: root_path.clone(),
                    source: std::io::Error::other(source.to_string()),
                })?;
            if cancellation.load(Ordering::Acquire) {
                return Ok(None);
            }
            let (next_reader, batch) = tokio::task::spawn_blocking(move || {
                let _permit = read_permit;
                reader_to_move.next_batch(reader_batch_size)
            })
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

    pub(in crate::application::scanner) async fn prepare_manifest_discovery_positive_indexes(
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
        let mut directory_folder_provider_ids: Option<Arc<OnceLock<BTreeMap<String, String>>>> =
            None;
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
            let is_sidecar = super::super::library::is_supported_incremental_sidecar_file(&path);
            let baseline = baselines.get(&observation.relative_path);
            if let Some(baseline) = baseline {
                if baseline.entry_kind != "FILE" {
                    continue;
                }
                if !baseline.is_missing
                    && baseline.fingerprint.as_deref() == Some(observation.fingerprint.as_slice())
                {
                    let inferred_suffix = if is_media && library_kind == "MOVIE" {
                        infer_sibling_movie_variant_suffix(&path).await
                    } else {
                        None
                    };
                    let variant_identity_is_current = match inferred_suffix.as_deref() {
                        Some(suffix) => {
                            let base_item_id = self
                                .scanner
                                .find_inferred_movie_variant_base_item(
                                    &root.id, &root_path, &path, suffix,
                                )
                                .await?;
                            if let Some(base_item_id) = base_item_id {
                                self.database
                                    .scan_manifest_movie_variant_identity_is_current(
                                        &root.id,
                                        &observation.relative_path,
                                        &base_item_id,
                                        Some(suffix),
                                    )
                                    .await?
                            } else {
                                false
                            }
                        }
                        None => false,
                    };
                    if inferred_suffix.is_none() || variant_identity_is_current {
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
            let filename_input = match library_kind {
                "MOVIE" => ManifestFilenameInput::MovieLibrary,
                "SERIES" => ManifestFilenameInput::SeriesLibrary,
                "HOMEVIDEOS" => ManifestFilenameInput::HomeVideos,
                _ if is_media => ManifestFilenameInput::PreparedMixed(
                    classify_mixed_manifest_file(&root_path, &path, &mut classification_cache)
                        .await,
                ),
                _ => ManifestFilenameInput::LegacyMixed(None),
            };
            if let Some(started) = classification_started {
                classification_duration = classification_duration.saturating_add(started.elapsed());
            }
            classification_count = classification_count.saturating_add(1);
            let movie_folder_provider_ids = if is_media && filename_input.is_movie() {
                Some(Arc::clone(
                    directory_folder_provider_ids.get_or_insert_with(|| Arc::new(OnceLock::new())),
                ))
            } else {
                None
            };
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
                        movie_folder_provider_ids_cache: movie_folder_provider_ids,
                        verify_path_after_preparation: false,
                    },
                    observed,
                    is_add,
                    None,
                    filename_input,
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

    pub(in crate::application::scanner) async fn discover_scan_manifest_directory_group_batches(
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
            let open_permit = acquire_process_background_work(BackgroundWorkStage::Discovery)
                .await
                .map_err(|source| ScannerError::Io {
                    path: root_path.clone(),
                    source: std::io::Error::other(source.to_string()),
                })?;
            if cancellation.load(Ordering::Acquire) {
                return Ok(None);
            }
            let mut reader = tokio::task::spawn_blocking(move || {
                let _permit = open_permit;
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
                let read_permit = acquire_process_background_work(BackgroundWorkStage::Discovery)
                    .await
                    .map_err(|source| ScannerError::Io {
                        path: root_path.clone(),
                        source: std::io::Error::other(source.to_string()),
                    })?;
                if cancellation.load(Ordering::Acquire) {
                    return Ok(None);
                }
                let (next_reader, batch) = tokio::task::spawn_blocking(move || {
                    let _permit = read_permit;
                    reader.next_batch(reader_batch_size)
                })
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

    pub(in crate::application::scanner) async fn commit_pending_manifest_directory_chunks(
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
            skip_baseline_queries,
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
        let baselines =
            if stream_files_during_discovery && !skip_baseline_queries && !file_paths.is_empty() {
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
        let legacy_variant_paths = stored_chunks
            .iter()
            .flat_map(|chunk| chunk.positive_indexes.iter())
            .filter(|positive| positive.delta_kind != "ADD")
            .filter_map(|positive| match &positive.file {
                NewScanManifestIndexedFile::Movie(file) => Some(file.relative_path.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
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
                let movie_identity_reassigned = self
                    .reconcile_legacy_movie_variant_paths(
                        job_id,
                        &root.id,
                        Path::new(&root.canonical_path),
                        &legacy_variant_paths,
                    )
                    .await?;
                self.publish_home_after_manifest_commit(&result, movie_identity_reassigned)
                    .await;
                if result.local_metadata_batches_changed {
                    self.notify_local_metadata_outbox_worker();
                }
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

    pub(in crate::application::scanner) async fn reconcile_legacy_movie_variant_paths(
        &self,
        job_id: &str,
        library_root_id: &str,
        root_path: &Path,
        relative_paths: &[String],
    ) -> Result<bool, ScannerError> {
        if relative_paths.is_empty() {
            return Ok(false);
        }
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Ok(false);
        };
        let Some(root) = self.database.find_library_root(library_root_id).await? else {
            return Ok(false);
        };
        let mut movie_identity_reassigned = false;
        for relative_path in relative_paths {
            let path = root_path.join(relative_path);
            let inferred_suffix = infer_sibling_movie_variant_suffix(&path).await;
            if inferred_suffix.is_none() {
                continue;
            }
            let report = self
                .scanner
                .scan_movie_file_with_inferred_suffix(
                    &job.library_id,
                    &root,
                    root_path,
                    &path,
                    &job.generation,
                    inferred_suffix.as_deref(),
                )
                .await?;
            movie_identity_reassigned |= report.changed_files > 0;
        }
        Ok(movie_identity_reassigned)
    }

    pub(in crate::application::scanner) async fn commit_scan_manifest_discovery_chunk(
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
                root_path.clone(),
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
                let legacy_variant_paths = chunk
                    .positive_indexes
                    .iter()
                    .filter(|positive| positive.delta_kind != "ADD")
                    .filter_map(|positive| match &positive.file {
                        NewScanManifestIndexedFile::Movie(file) => Some(file.relative_path.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let movie_identity_reassigned = self
                    .reconcile_legacy_movie_variant_paths(
                        chunk.job_id,
                        chunk.library_root_id,
                        &root_path,
                        &legacy_variant_paths,
                    )
                    .await?;
                self.publish_home_after_manifest_commit(&result, movie_identity_reassigned)
                    .await;
                if result.local_metadata_batches_changed {
                    self.notify_local_metadata_outbox_worker();
                }
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

    pub(in crate::application::scanner) async fn publish_home_after_manifest_commit(
        &self,
        result: &ManifestDiscoveryCommitResult,
        additional_content_changed: bool,
    ) {
        if result.created_items == 0
            && !result.local_metadata_batches_changed
            && !result.metadata_targets_changed
            && !additional_content_changed
        {
            return;
        }
        if let Some(home) = &self.home {
            home.invalidate();
        }
        self.user_events.publish_home_coalesced().await;
    }

    pub(in crate::application::scanner) async fn run_lite_scan_manifest_discovery_batch(
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
        let mut directories_by_root = BTreeMap::<String, (bool, Vec<String>)>::new();
        for (root_id, relative_directory, has_filesystem_entries) in directories {
            directories_by_root
                .entry(root_id)
                .or_insert_with(|| (has_filesystem_entries, Vec::new()))
                .1
                .push(relative_directory);
        }
        let roots_by_id = self
            .database
            .list_library_roots_by_ids(&directories_by_root.keys().cloned().collect::<Vec<_>>())
            .await?;
        let mut discovered_count = job.total_count;
        let mut created_items = 0_usize;

        for (root_id, (has_filesystem_entries, relative_directories)) in directories_by_root {
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
                skip_baseline_queries: !has_filesystem_entries,
                library_kind: &library.kind,
                preparation_concurrency,
                expected_root_identity,
            };
            let discovery = self
                .discover_scan_manifest_directory_group_batches(context, &relative_directories)
                .await;
            match discovery {
                Ok(Some(discovered)) => {
                    discovered_count = discovered_count.saturating_add(
                        i64::try_from(discovered.observed_file_count).unwrap_or(i64::MAX),
                    );
                    created_items = created_items.saturating_add(discovered.created_items);
                    self.push_lite_manifest_directories(
                        manifest_id,
                        &root_id,
                        has_filesystem_entries,
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

    pub(in crate::application::scanner) async fn run_scan_manifest_discovery_batch(
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
                skip_baseline_queries: false,
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
}
