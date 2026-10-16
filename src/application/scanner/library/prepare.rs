use super::*;

impl LibraryScanner {
    pub(in crate::application::scanner) async fn prepare_new_movie_file(
        &self,
        root_path: &Path,
        path: &Path,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        self.prepare_new_movie_file_with_folder_provider_ids(root_path, path, None)
            .await
    }

    pub(in crate::application::scanner) async fn prepare_new_movie_file_with_folder_provider_ids(
        &self,
        root_path: &Path,
        path: &Path,
        folder_provider_ids: Option<&BTreeMap<String, String>>,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        self.prepare_new_movie_file_with_strm_target(root_path, path, folder_provider_ids, None)
            .await
    }

    pub(in crate::application::scanner) async fn prepare_manifest_movie_file(
        &self,
        path: &Path,
        observation: &NewScanManifestEntry,
        strm_target: Option<StrmTarget>,
        parsed_name: ParsedMovieFilename,
        folder_provider_ids: Option<&BTreeMap<String, String>>,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        self.prepare_new_movie_file_with_manifest_observation(
            path,
            observation,
            strm_target,
            parsed_name,
            folder_provider_ids,
        )
        .await
    }

    pub(in crate::application::scanner) async fn prepare_new_movie_file_with_manifest_observation(
        &self,
        path: &Path,
        observation: &NewScanManifestEntry,
        manifest_strm_target: Option<StrmTarget>,
        parsed_name: ParsedMovieFilename,
        folder_provider_ids: Option<&BTreeMap<String, String>>,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        let provider_ids = folder_provider_ids.map_or_else(
            || movie_provider_ids(path, &parsed_name.provider_ids),
            |folder_provider_ids| {
                merge_movie_provider_ids(parsed_name.provider_ids.clone(), folder_provider_ids)
            },
        );
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
            device: observation.device,
            inode: observation.inode,
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

    pub(in crate::application::scanner) async fn prepare_new_movie_file_with_strm_target(
        &self,
        root_path: &Path,
        path: &Path,
        folder_provider_ids: Option<&BTreeMap<String, String>>,
        manifest_strm_target: Option<StrmTarget>,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let inferred_suffix = infer_sibling_movie_variant_suffix(path).await;
        let parsed_name = inferred_suffix
            .as_deref()
            .and_then(|suffix| parse_movie_filename_with_variant_suffix(file_name, suffix))
            .or_else(|| parse_movie_filename(file_name));
        let Some(parsed_name) = parsed_name else {
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
            compute_scan_fingerprint_v2(&relative_path, size, modified_at, device, inode);
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
            device: device.and_then(|value| i64::try_from(value).ok()),
            inode: inode.and_then(|value| i64::try_from(value).ok()),
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

    pub(in crate::application::scanner) async fn prepare_new_episode_file(
        &self,
        root_id: &str,
        root_path: &Path,
        path: &Path,
    ) -> Result<Option<NewEpisodeFile>, ScannerError> {
        self.prepare_new_episode_file_with_strm_target(root_id, root_path, path, None)
            .await
    }

    pub(in crate::application::scanner) async fn prepare_manifest_episode_file(
        &self,
        root_id: &str,
        path: &Path,
        observation: &NewScanManifestEntry,
        strm_target: Option<StrmTarget>,
        parsed: ParsedEpisodeFilename,
    ) -> Result<Option<NewEpisodeFile>, ScannerError> {
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
            device: observation.device,
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

    pub(in crate::application::scanner) async fn prepare_new_episode_file_with_strm_target(
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
            compute_scan_fingerprint_v2(&relative_path, size, modified_at, device, inode);
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
            device: device.and_then(|value| i64::try_from(value).ok()),
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

    pub(in crate::application::scanner) async fn prepare_manifest_unresolved_file(
        &self,
        root: &StoredLibraryRoot,
        path: &Path,
        observation: &NewScanManifestEntry,
        manifest_strm_target: Option<StrmTarget>,
    ) -> Result<NewScanManifestUnresolvedFile, ScannerError> {
        self.prepare_manifest_plain_file(root, path, observation, manifest_strm_target, false)
            .await
    }

    pub(in crate::application::scanner) async fn prepare_manifest_home_video_file(
        &self,
        root: &StoredLibraryRoot,
        path: &Path,
        observation: &NewScanManifestEntry,
        manifest_strm_target: Option<StrmTarget>,
    ) -> Result<NewScanManifestUnresolvedFile, ScannerError> {
        self.prepare_manifest_plain_file(root, path, observation, manifest_strm_target, true)
            .await
    }

    pub(in crate::application::scanner) async fn prepare_manifest_plain_file(
        &self,
        root: &StoredLibraryRoot,
        path: &Path,
        observation: &NewScanManifestEntry,
        manifest_strm_target: Option<StrmTarget>,
        home_video: bool,
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
            .unwrap_or(if home_video { "Video" } else { "Unresolved" });
        let cleaned_title = clean_hierarchy_title(file_stem);
        let title = if cleaned_title.is_empty() {
            if home_video {
                "Video".to_owned()
            } else {
                "Unresolved".to_owned()
            }
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
            identity_key: format!(
                "{}:{}:{relative_path}",
                if home_video { "video" } else { "unresolved" },
                root.id
            ),
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
            home_video,
        })
    }

    pub(in crate::application::scanner) async fn prepare_new_episode_files(
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

    pub(in crate::application::scanner) async fn prepare_new_episode_files_with_concurrency(
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

    pub(in crate::application::scanner) async fn prepare_new_movie_files(
        &self,
        root_path: &Path,
        paths: &[PathBuf],
    ) -> Result<Vec<Option<NewMovieFile>>, ScannerError> {
        self.prepare_new_movie_files_with_concurrency(root_path, paths, self.scan_concurrency)
            .await
    }

    pub(in crate::application::scanner) async fn prepare_new_movie_files_with_concurrency(
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

    pub(in crate::application::scanner) async fn flush_new_movie_files(
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
        let inferred_suffix = infer_sibling_movie_variant_suffix(path).await;
        self.scan_movie_file_with_inferred_suffix(
            library_id_text,
            root,
            root_path,
            path,
            generation,
            inferred_suffix.as_deref(),
        )
        .await
    }

    pub(in crate::application::scanner) async fn scan_movie_file_with_inferred_suffix(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        generation: &str,
        inferred_suffix: Option<&str>,
    ) -> Result<ScanReport, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(ScanReport::default());
        };
        let parsed_name = inferred_suffix
            .and_then(|suffix| parse_movie_filename_with_variant_suffix(file_name, suffix))
            .or_else(|| parse_movie_filename(file_name));
        let Some(parsed_name) = parsed_name else {
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
        let device = device.and_then(|value| i64::try_from(value).ok());
        let inode = inode.and_then(|value| i64::try_from(value).ok());
        let mut report = ScanReport {
            discovered_files: 1,
            ..ScanReport::default()
        };
        let existing_entry = self
            .database
            .find_filesystem_entry(&root.id, &relative_path)
            .await?;
        let fingerprint = existing_entry
            .as_ref()
            .filter(|entry| {
                scan_fingerprint_matches_observation(
                    entry,
                    &relative_path,
                    size,
                    modified_at,
                    device.and_then(|value| u64::try_from(value).ok()),
                    inode.and_then(|value| u64::try_from(value).ok()),
                )
            })
            .and_then(|entry| entry.fingerprint.clone())
            .unwrap_or_else(|| {
                compute_scan_fingerprint_v2(
                    &relative_path,
                    size,
                    modified_at,
                    device.and_then(|value| u64::try_from(value).ok()),
                    inode.and_then(|value| u64::try_from(value).ok()),
                )
            });
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
        if inferred_suffix.is_none()
            && !has_multi_part_marker(file_name)
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
                    .mark_filesystem_entry_seen(&existing_entry.id, generation, device, inode)
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
                    device,
                    inode,
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

        // NFO or online metadata can replace the parsed title and year. The exact
        // sibling base's indexed item remains the identity anchor for this version.
        let base_item_id = match inferred_suffix {
            Some(suffix) => {
                self.find_inferred_movie_variant_base_item(&root.id, root_path, path, suffix)
                    .await?
            }
            None => None,
        };
        let existing_item_id = match base_item_id {
            Some(item_id) => Some(item_id),
            None => self
                .database
                .find_media_item(
                    library_id_text,
                    &parsed_name.sort_title,
                    parsed_name.production_year.map(i64::from),
                )
                .await?
                .map(|item| item.id),
        };
        let (item_id, created_item) = if let Some(item_id) = existing_item_id {
            self.database.restore_media_item(&item_id).await?;
            (item_id, false)
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
                    .mark_filesystem_entry_seen(&existing_entry.id, generation, device, inode)
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
                    device,
                    inode,
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
                device: device.and_then(|value| i64::try_from(value).ok()),
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

    pub(in crate::application::scanner) async fn find_inferred_movie_variant_base_item(
        &self,
        library_root_id: &str,
        root_path: &Path,
        path: &Path,
        suffix: &str,
    ) -> Result<Option<String>, ScannerError> {
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            return Ok(None);
        };
        let Some(base) = stem.strip_suffix(&format!("-{suffix}")) else {
            return Ok(None);
        };
        let Some(directory) = path.parent() else {
            return Ok(None);
        };
        let relative_paths = movie_variant_extensions(path)
            .into_iter()
            .map(|extension| {
                let sibling = directory.join(format!("{base}.{extension}"));
                sibling
                    .strip_prefix(root_path)
                    .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                    .to_str()
                    .map(str::to_owned)
                    .ok_or(ScannerError::NonUtf8Path)
            })
            .collect::<Result<Vec<_>, ScannerError>>()?;
        Ok(self
            .database
            .find_movie_item_for_filesystem_paths(library_root_id, &relative_paths)
            .await?)
    }
}
