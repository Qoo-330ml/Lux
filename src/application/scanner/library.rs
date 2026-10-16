use super::*;

mod prepare;

pub(in crate::application::scanner) type MoviePreparationOutput = (
    usize,
    String,
    StoredReconciliationScanEntry,
    Option<NewMovieFile>,
);
pub(in crate::application::scanner) type MoviePreparationTask =
    Result<MoviePreparationOutput, ScannerError>;
pub(in crate::application::scanner) type MovieFingerprintTask =
    Result<(usize, MovieQuickScanResult), ScannerError>;
pub(in crate::application::scanner) type EpisodeQuickResult =
    Option<(String, ScanReport, Option<(String, String)>)>;
pub(in crate::application::scanner) type EpisodeFingerprintTask =
    Result<(usize, EpisodeQuickResult), ScannerError>;
pub(in crate::application::scanner) type ReconciliationFingerprintTask =
    Result<(usize, Option<(String, ScanReport)>), ScannerError>;
pub(in crate::application::scanner) type ReconciliationRegularTask =
    Result<(usize, ScanReport), ScannerError>;
pub(in crate::application::scanner) type ReconciliationRegularGroupTask =
    Result<Vec<(usize, ScanReport)>, ScannerError>;

pub(in crate::application::scanner) struct ReconciliationScanWork {
    pub(in crate::application::scanner) entry: StoredReconciliationScanEntry,
    pub(in crate::application::scanner) path: PathBuf,
    pub(in crate::application::scanner) classification: MixedClassification,
}

#[derive(Clone)]
pub(in crate::application::scanner) struct ReconciliationRegularWork {
    pub(in crate::application::scanner) index: usize,
    pub(in crate::application::scanner) entry: StoredReconciliationScanEntry,
    pub(in crate::application::scanner) root: StoredLibraryRoot,
    pub(in crate::application::scanner) path: PathBuf,
    pub(in crate::application::scanner) classification: MixedClassification,
}

pub(in crate::application::scanner) enum MovieQuickScanResult {
    Unchanged {
        entry_id: String,
        report: ScanReport,
    },
    Rescan,
    RescanWithVariantSuffix(String),
}

pub(in crate::application::scanner) fn reconciliation_regular_group_key(
    root_id: &str,
    path: &Path,
    classification: MixedClassification,
    inferred_suffix: Option<&str>,
    fallback_index: usize,
) -> String {
    let parsed_key = match classification {
        MixedClassification::Movie => path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| {
                inferred_suffix
                    .and_then(|suffix| parse_movie_filename_with_variant_suffix(name, suffix))
                    .or_else(|| parse_movie_filename(name))
            })
            .map(|parsed| {
                format!(
                    "movie:{}:{:?}:{:?}",
                    parsed.sort_title, parsed.production_year, parsed.provider_ids
                )
            }),
        MixedClassification::Episode => path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(parse_episode_filename)
            .map(|parsed| {
                format!(
                    "episode:{}:{:?}:{:?}:{:?}",
                    parsed.sort_title, parsed.production_year, parsed.season, parsed.episode
                )
            }),
        MixedClassification::Unresolved => None,
    };
    parsed_key.map_or_else(
        || format!("path:{root_id}:{fallback_index}"),
        |key| format!("{root_id}:{key}"),
    )
}

pub(in crate::application::scanner) async fn collect_movie_fingerprint_task(
    tasks: &mut JoinSet<MovieFingerprintTask>,
    results: &mut [MovieQuickScanResult],
) -> Result<(), ScannerError> {
    let (index, result) = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<movie-fingerprint-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<movie-fingerprint-task>"),
                source: std::io::Error::other("movie fingerprint task set is empty"),
            });
        }
    };
    if let Some(slot) = results.get_mut(index) {
        *slot = result;
    }
    Ok(())
}

pub(in crate::application::scanner) async fn collect_episode_fingerprint_task(
    tasks: &mut JoinSet<EpisodeFingerprintTask>,
    results: &mut [EpisodeQuickResult],
) -> Result<(), ScannerError> {
    let (index, result) = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<episode-fingerprint-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<episode-fingerprint-task>"),
                source: std::io::Error::other("episode fingerprint task set is empty"),
            });
        }
    };
    if let Some(slot) = results.get_mut(index) {
        *slot = result;
    }
    Ok(())
}

pub(in crate::application::scanner) async fn collect_reconciliation_fingerprint_task(
    tasks: &mut JoinSet<ReconciliationFingerprintTask>,
    results: &mut [Option<(String, ScanReport)>],
) -> Result<(), ScannerError> {
    let (index, result) = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<reconciliation-fingerprint-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<reconciliation-fingerprint-task>"),
                source: std::io::Error::other("reconciliation fingerprint task set is empty"),
            });
        }
    };
    if let Some(slot) = results.get_mut(index) {
        *slot = result;
    }
    Ok(())
}

pub(in crate::application::scanner) async fn collect_reconciliation_regular_task(
    tasks: &mut JoinSet<ReconciliationRegularTask>,
    results: &mut [Option<ScanReport>],
) -> Result<(), ScannerError> {
    let (index, result) = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<reconciliation-regular-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<reconciliation-regular-task>"),
                source: std::io::Error::other("reconciliation regular task set is empty"),
            });
        }
    };
    if let Some(slot) = results.get_mut(index) {
        *slot = Some(result);
    }
    Ok(())
}

pub(in crate::application::scanner) async fn collect_reconciliation_regular_group_task(
    tasks: &mut JoinSet<ReconciliationRegularGroupTask>,
    results: &mut [Option<ScanReport>],
) -> Result<(), ScannerError> {
    let reports = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<reconciliation-regular-group-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<reconciliation-regular-group-task>"),
                source: std::io::Error::other("reconciliation regular group task set is empty"),
            });
        }
    };
    for (index, report) in reports {
        if let Some(slot) = results.get_mut(index) {
            *slot = Some(report);
        }
    }
    Ok(())
}

pub(in crate::application::scanner) async fn join_movie_preparation(
    tasks: &mut JoinSet<MoviePreparationTask>,
) -> Result<MoviePreparationOutput, ScannerError> {
    match tasks.join_next().await {
        Some(Ok(result)) => result,
        Some(Err(error)) => Err(ScannerError::Io {
            path: PathBuf::from("<scan-preparation-task>"),
            source: std::io::Error::other(error.to_string()),
        }),
        None => Err(ScannerError::Io {
            path: PathBuf::from("<scan-preparation-task>"),
            source: std::io::Error::other("scan preparation task set is empty"),
        }),
    }
}

pub(in crate::application::scanner) async fn collect_movie_preparation_task(
    tasks: &mut JoinSet<Result<(usize, Option<NewMovieFile>), ScannerError>>,
    results: &mut Vec<(usize, Option<NewMovieFile>)>,
) -> Result<(), ScannerError> {
    let result = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<movie-preparation-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<movie-preparation-task>"),
                source: std::io::Error::other("movie preparation task set is empty"),
            });
        }
    };
    results.push(result);
    Ok(())
}

pub(in crate::application::scanner) async fn collect_episode_preparation_task(
    tasks: &mut JoinSet<Result<(usize, Option<NewEpisodeFile>), ScannerError>>,
    results: &mut [Option<NewEpisodeFile>],
) -> Result<(), ScannerError> {
    let (index, result) = match tasks.join_next().await {
        Some(Ok(result)) => result?,
        Some(Err(error)) => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<episode-preparation-task>"),
                source: std::io::Error::other(error.to_string()),
            });
        }
        None => {
            return Err(ScannerError::Io {
                path: PathBuf::from("<episode-preparation-task>"),
                source: std::io::Error::other("episode preparation task set is empty"),
            });
        }
    };
    if let Some(slot) = results.get_mut(index) {
        *slot = result;
    }
    Ok(())
}

pub(super) struct FileBatchWalker {
    directories: Vec<PathBuf>,
    current: Option<(PathBuf, fs::ReadDir)>,
    include_sidecars: bool,
}

impl FileBatchWalker {
    pub(super) fn new(root: &Path) -> Self {
        Self {
            directories: vec![root.to_owned()],
            current: None,
            include_sidecars: false,
        }
    }

    pub(super) fn new_with_sidecars(root: &Path) -> Self {
        Self {
            directories: vec![root.to_owned()],
            current: None,
            include_sidecars: true,
        }
    }

    pub(super) async fn next_batch(
        &mut self,
        batch_size: usize,
    ) -> Result<Option<Vec<PathBuf>>, ScannerError> {
        if batch_size == 0 {
            return Ok(None);
        }
        let mut files = Vec::with_capacity(batch_size);
        while files.len() < batch_size {
            if self.current.is_none() {
                let Some(directory) = self.directories.pop() else {
                    break;
                };
                let entries =
                    fs::read_dir(&directory)
                        .await
                        .map_err(|source| ScannerError::Io {
                            path: directory.clone(),
                            source,
                        })?;
                self.current = Some((directory, entries));
            }

            let Some((directory, entries)) = self.current.as_mut() else {
                continue;
            };
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    let path = entry.path();
                    let file_type = entry.file_type().await.map_err(|source| ScannerError::Io {
                        path: path.clone(),
                        source,
                    })?;
                    if file_type.is_file()
                        && (is_supported_movie_file(&path)
                            || (self.include_sidecars
                                && is_supported_incremental_sidecar_file(&path)))
                    {
                        files.push(path);
                    } else if file_type.is_dir() {
                        self.directories.push(path);
                    }
                }
                Ok(None) => self.current = None,
                Err(source) => {
                    return Err(ScannerError::Io {
                        path: directory.clone(),
                        source,
                    });
                }
            }
        }
        if files.is_empty() {
            Ok(None)
        } else {
            files.sort();
            Ok(Some(files))
        }
    }
}

#[derive(Clone)]
pub struct LibraryScanner {
    database: Database,
    pub(super) scan_concurrency: usize,
}

struct EnsuredEpisodeHierarchy {
    episode_id: String,
    created_items: usize,
    episode_created: bool,
}

impl LibraryScanner {
    pub fn new(database: Database) -> Self {
        let scan_concurrency =
            usize::try_from(scan_concurrency_from_env().unwrap_or(DEFAULT_SCAN_CONCURRENCY))
                .unwrap_or(1)
                .max(1);
        Self {
            database,
            scan_concurrency,
        }
    }

    pub async fn repair_legacy_identity_keys(&self) -> Result<usize, ScannerError> {
        if self.database.identity_stability_repair_completed().await? {
            return Ok(0);
        }
        let candidates = self
            .database
            .list_episode_identity_repair_candidates()
            .await?;
        let mut repaired = 0;
        for candidate in candidates {
            if self.repair_identity_candidate(candidate).await? {
                repaired += 1;
            }
        }
        self.database
            .mark_identity_stability_repair_completed()
            .await?;
        Ok(repaired)
    }

    async fn repair_identity_candidate(
        &self,
        candidate: StoredEpisodeIdentityCandidate,
    ) -> Result<bool, ScannerError> {
        let Some(root) = self
            .database
            .find_library_root(&candidate.library_root_id)
            .await?
        else {
            return Ok(false);
        };
        let Some(file_name) = Path::new(&candidate.relative_path)
            .file_name()
            .and_then(|value| value.to_str())
        else {
            return Ok(false);
        };
        let Some(parsed) = parse_episode_filename(file_name) else {
            return Ok(false);
        };
        if let Ok(metadata) =
            fs::metadata(Path::new(&root.canonical_path).join(&candidate.relative_path)).await
        {
            let (_, inode) = file_identity(&metadata);
            if let Some(inode) = inode.and_then(|value| i64::try_from(value).ok()) {
                self.database
                    .update_filesystem_entry_inode(&candidate.filesystem_entry_id, Some(inode))
                    .await?;
            }
        }
        let hierarchy = episode_hierarchy(&candidate.relative_path, &parsed);
        let series_identity = format!("series:{}:{}", root.id, hierarchy.series_path);
        let season_identity = format!("{series_identity}:season:{}", hierarchy.season_number);
        let episode_identity = Self::episode_identity_key(&root, &hierarchy, &parsed);
        self.database
            .repair_episode_hierarchy_identities(
                &candidate.episode_id,
                &series_identity,
                &season_identity,
                &episode_identity,
            )
            .await
            .map_err(ScannerError::from)
    }

    pub async fn scan_movie_library(
        &self,
        library_id: LibraryId,
    ) -> Result<ScanReport, ScannerError> {
        let library_id_text = library_id.to_string();
        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_none()
        {
            return Err(ScannerError::LibraryNotFound);
        }

        let generation = Uuid::now_v7().to_string();
        let roots = self.database.list_library_roots(&library_id_text).await?;
        let mut report = ScanReport::default();
        for root in roots {
            let root_path = PathBuf::from(&root.canonical_path);
            let root_is_available = fs::metadata(&root_path)
                .await
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false);
            if !root_is_available {
                self.database
                    .update_library_root_availability(&root.id, false)
                    .await?;
                report.unavailable_roots += 1;
                continue;
            }
            if !root.is_available {
                self.database
                    .update_library_root_availability(&root.id, true)
                    .await?;
            }
            let mut walker = FileBatchWalker::new(&root_path);
            while let Some(files) = walker.next_batch(FILE_BATCH_SIZE).await? {
                report.merge(
                    self.scan_movie_file_batch(
                        &library_id_text,
                        &root,
                        &root_path,
                        &files,
                        &generation,
                    )
                    .await?,
                );
            }
            report.marked_missing += usize::try_from(
                self.database
                    .mark_missing_filesystem_entries(&root.id, &generation)
                    .await?,
            )
            .unwrap_or(usize::MAX);
        }
        Ok(report)
    }

    pub async fn scan_series_library(
        &self,
        library_id: LibraryId,
    ) -> Result<ScanReport, ScannerError> {
        let library_id_text = library_id.to_string();
        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_none()
        {
            return Err(ScannerError::LibraryNotFound);
        }
        let generation = Uuid::now_v7().to_string();
        let roots = self.database.list_library_roots(&library_id_text).await?;
        let mut report = ScanReport::default();
        let mut refreshed_series = HashSet::new();
        for root in roots {
            let root_path = PathBuf::from(&root.canonical_path);
            let root_is_available = fs::metadata(&root_path)
                .await
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false);
            if !root_is_available {
                self.database
                    .update_library_root_availability(&root.id, false)
                    .await?;
                report.unavailable_roots += 1;
                continue;
            }
            if !root.is_available {
                self.database
                    .update_library_root_availability(&root.id, true)
                    .await?;
            }
            let mut walker = FileBatchWalker::new(&root_path);
            while let Some(files) = walker.next_batch(FILE_BATCH_SIZE).await? {
                let relative_paths = files
                    .iter()
                    .map(|path| {
                        path.strip_prefix(&root_path)
                            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                            .to_str()
                            .map(str::to_owned)
                            .ok_or(ScannerError::NonUtf8Path)
                    })
                    .collect::<Result<Vec<_>, ScannerError>>()?;
                let existing_entries = self
                    .database
                    .list_filesystem_entries_for_paths(&root.id, &relative_paths)
                    .await?;
                let mut seen_entry_ids = Vec::with_capacity(files.len());
                let mut new_paths = Vec::new();
                let quick_results = self
                    .scan_episode_files_if_unchanged(
                        &root,
                        &root_path,
                        &files,
                        &existing_entries,
                        self.scan_concurrency,
                    )
                    .await?;
                for (path, quick_result) in files.into_iter().zip(quick_results) {
                    if let Some((entry_id, quick_report, provider_update)) = quick_result {
                        if let Some((series_identity, provider_ids_json)) = provider_update
                            && refreshed_series.insert(series_identity.clone())
                        {
                            self.database
                                .update_local_provider_ids_for_identity_if_empty(
                                    &series_identity,
                                    &provider_ids_json,
                                )
                                .await?;
                        }
                        seen_entry_ids.push(entry_id);
                        report.merge(quick_report);
                        continue;
                    }
                    let relative_path = path
                        .strip_prefix(&root_path)
                        .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                        .to_str()
                        .ok_or(ScannerError::NonUtf8Path)?;
                    if !existing_entries.contains_key(relative_path) {
                        let requires_regular_scan = self
                            .file_has_moved_entry(&library_id_text, &root, &root_path, &path)
                            .await?
                            || self
                                .episode_path_has_legacy_identity(&root, &root_path, &path)
                                .await?;
                        if requires_regular_scan {
                            report.merge(
                                self.scan_episode_file_with_provider_cache(
                                    &library_id_text,
                                    &root,
                                    &root_path,
                                    &path,
                                    &generation,
                                    Some(&mut refreshed_series),
                                )
                                .await?,
                            );
                        } else {
                            new_paths.push(path);
                        }
                        continue;
                    }
                    report.merge(
                        self.scan_episode_file_with_provider_cache(
                            &library_id_text,
                            &root,
                            &root_path,
                            &path,
                            &generation,
                            Some(&mut refreshed_series),
                        )
                        .await?,
                    );
                }
                let new_episode_files = self
                    .prepare_new_episode_files(&root, &root_path, &new_paths)
                    .await?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                if !new_episode_files.is_empty() {
                    let file_count = new_episode_files.len();
                    report.created_items += self
                        .database
                        .insert_episode_files_batch(
                            &library_id_text,
                            &root.id,
                            &generation,
                            &new_episode_files,
                        )
                        .await?;
                    report.discovered_files += file_count;
                    report.created_sources += file_count;
                }
                self.database
                    .mark_filesystem_entries_seen_batch(&seen_entry_ids, &generation)
                    .await?;
            }
            report.marked_missing += usize::try_from(
                self.database
                    .mark_missing_filesystem_entries(&root.id, &generation)
                    .await?,
            )
            .unwrap_or(usize::MAX);
        }
        Ok(report)
    }

    pub async fn scan_mixed_library(
        &self,
        library_id: LibraryId,
    ) -> Result<ScanReport, ScannerError> {
        let library_id_text = library_id.to_string();
        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_none()
        {
            return Err(ScannerError::LibraryNotFound);
        }
        let generation = Uuid::now_v7().to_string();
        let roots = self.database.list_library_roots(&library_id_text).await?;
        let mut report = ScanReport::default();
        let mut refreshed_series = HashSet::new();
        for root in roots {
            let root_path = PathBuf::from(&root.canonical_path);
            let root_is_available = fs::metadata(&root_path)
                .await
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false);
            if !root_is_available {
                self.database
                    .update_library_root_availability(&root.id, false)
                    .await?;
                report.unavailable_roots += 1;
                continue;
            }
            if !root.is_available {
                self.database
                    .update_library_root_availability(&root.id, true)
                    .await?;
            }
            let mut walker = FileBatchWalker::new(&root_path);
            while let Some(files) = walker.next_batch(FILE_BATCH_SIZE).await? {
                let relative_paths = files
                    .iter()
                    .map(|path| {
                        path.strip_prefix(&root_path)
                            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                            .to_str()
                            .map(str::to_owned)
                            .ok_or(ScannerError::NonUtf8Path)
                    })
                    .collect::<Result<Vec<_>, ScannerError>>()?;
                let existing_entries = self
                    .database
                    .list_filesystem_entries_for_paths(&root.id, &relative_paths)
                    .await?;
                let mut seen_entry_ids = Vec::with_capacity(files.len());
                let mut new_movie_paths = Vec::new();
                let mut new_episode_paths = Vec::new();
                let mut classification_cache = MixedClassificationCache::default();
                for path in files {
                    let classification =
                        classify_mixed_file(&root_path, &path, &mut classification_cache).await;
                    let mut inferred_movie_variant_suffix = None;
                    let quick_report = match classification {
                        MixedClassification::Movie => {
                            let relative_path = path
                                .strip_prefix(&root_path)
                                .map_err(|error| {
                                    ScannerError::InvalidRelativePath(error.to_string())
                                })?
                                .to_str()
                                .ok_or(ScannerError::NonUtf8Path)?;
                            match existing_entries.get(relative_path) {
                                Some(existing_entry) => {
                                    let quick_result = self
                                        .scan_movie_file_if_unchanged(
                                            &root.id,
                                            &root_path,
                                            &path,
                                            existing_entry,
                                        )
                                        .await?;
                                    match quick_result {
                                        MovieQuickScanResult::Unchanged { entry_id, report } => {
                                            Some((entry_id, report))
                                        }
                                        MovieQuickScanResult::Rescan => None,
                                        MovieQuickScanResult::RescanWithVariantSuffix(suffix) => {
                                            inferred_movie_variant_suffix = Some(suffix);
                                            None
                                        }
                                    }
                                }
                                None => None,
                            }
                        }
                        MixedClassification::Episode => {
                            self.scan_episode_file_if_unchanged(
                                &root,
                                &root_path,
                                &path,
                                &existing_entries,
                            )
                            .await?
                        }
                        MixedClassification::Unresolved => {
                            self.scan_unresolved_file_if_unchanged(
                                &root,
                                &root_path,
                                &path,
                                &existing_entries,
                            )
                            .await?
                        }
                    };
                    if let Some((entry_id, quick_report)) = quick_report {
                        seen_entry_ids.push(entry_id);
                        report.merge(quick_report);
                        continue;
                    }
                    let relative_path = path
                        .strip_prefix(&root_path)
                        .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                        .to_str()
                        .ok_or(ScannerError::NonUtf8Path)?;
                    if !existing_entries.contains_key(relative_path) {
                        let requires_regular_scan = match classification {
                            MixedClassification::Movie => {
                                self.file_has_moved_entry(
                                    &library_id_text,
                                    &root,
                                    &root_path,
                                    &path,
                                )
                                .await?
                            }
                            MixedClassification::Episode => {
                                self.file_has_moved_entry(
                                    &library_id_text,
                                    &root,
                                    &root_path,
                                    &path,
                                )
                                .await?
                                    || self
                                        .episode_path_has_legacy_identity(&root, &root_path, &path)
                                        .await?
                            }
                            MixedClassification::Unresolved => true,
                        };
                        if !requires_regular_scan {
                            let batched = match classification {
                                MixedClassification::Movie => {
                                    new_movie_paths.push(path.clone());
                                    true
                                }
                                MixedClassification::Episode => {
                                    new_episode_paths.push(path.clone());
                                    true
                                }
                                MixedClassification::Unresolved => false,
                            };
                            if batched {
                                continue;
                            }
                        }
                    }
                    let result = match classification {
                        MixedClassification::Movie => match inferred_movie_variant_suffix {
                            Some(suffix) => {
                                self.scan_movie_file_with_inferred_suffix(
                                    &library_id_text,
                                    &root,
                                    &root_path,
                                    &path,
                                    &generation,
                                    Some(&suffix),
                                )
                                .await?
                            }
                            None => {
                                self.scan_movie_file(
                                    &library_id_text,
                                    &root,
                                    &root_path,
                                    &path,
                                    &generation,
                                )
                                .await?
                            }
                        },
                        MixedClassification::Episode => {
                            self.scan_episode_file_with_provider_cache(
                                &library_id_text,
                                &root,
                                &root_path,
                                &path,
                                &generation,
                                Some(&mut refreshed_series),
                            )
                            .await?
                        }
                        MixedClassification::Unresolved => {
                            self.scan_unresolved_file(
                                &library_id_text,
                                &root,
                                &root_path,
                                &path,
                                &generation,
                            )
                            .await?
                        }
                    };
                    report.merge(result);
                }
                let new_movie_files = self
                    .prepare_new_movie_files_with_concurrency(
                        &root_path,
                        &new_movie_paths,
                        self.scan_concurrency,
                    )
                    .await?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                if !new_movie_files.is_empty() {
                    let file_count = new_movie_files.len();
                    report.created_items += self
                        .database
                        .insert_movie_files_batch(
                            &library_id_text,
                            &root.id,
                            &generation,
                            &new_movie_files,
                        )
                        .await?;
                    report.discovered_files += file_count;
                    report.created_sources += file_count;
                }
                let new_episode_files = self
                    .prepare_new_episode_files_with_concurrency(
                        &root,
                        &root_path,
                        &new_episode_paths,
                        self.scan_concurrency,
                    )
                    .await?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                if !new_episode_files.is_empty() {
                    let file_count = new_episode_files.len();
                    report.created_items += self
                        .database
                        .insert_episode_files_batch(
                            &library_id_text,
                            &root.id,
                            &generation,
                            &new_episode_files,
                        )
                        .await?;
                    report.discovered_files += file_count;
                    report.created_sources += file_count;
                }
                self.database
                    .mark_filesystem_entries_seen_batch(&seen_entry_ids, &generation)
                    .await?;
            }
            report.marked_missing += usize::try_from(
                self.database
                    .mark_missing_filesystem_entries(&root.id, &generation)
                    .await?,
            )
            .unwrap_or(usize::MAX);
        }
        Ok(report)
    }

    pub(super) async fn scan_episode_file(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        generation: &str,
    ) -> Result<ScanReport, ScannerError> {
        self.scan_episode_file_with_provider_cache(
            library_id_text,
            root,
            root_path,
            path,
            generation,
            None,
        )
        .await
    }

    async fn scan_episode_file_with_provider_cache(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        generation: &str,
        mut refreshed_series: Option<&mut HashSet<String>>,
    ) -> Result<ScanReport, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(ScanReport::default());
        };
        let Some(parsed) = parse_episode_filename(file_name) else {
            return Ok(ScanReport::default());
        };
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
        let existing_entry = self
            .database
            .find_filesystem_entry(&root.id, &relative_path)
            .await?;
        let mut existing_entry = existing_entry;
        let device = device.and_then(|value| i64::try_from(value).ok());
        let inode = inode.and_then(|value| i64::try_from(value).ok());
        if existing_entry.is_none()
            && let Some(matched_device) = device
            && let Some(matched_inode) = inode
            && let Some(entry) = self
                .database
                .find_filesystem_entry_by_inode(
                    library_id_text,
                    &root.id,
                    matched_device,
                    matched_inode,
                    &relative_path,
                )
                .await?
        {
            let fingerprint = compute_scan_fingerprint_v2(
                &relative_path,
                size,
                modified_at,
                u64::try_from(matched_device).ok(),
                u64::try_from(matched_inode).ok(),
            );
            self.database
                .move_filesystem_entry(FilesystemEntryMove {
                    entry_id: &entry.id,
                    library_root_id: &root.id,
                    relative_path: &relative_path,
                    size,
                    modified_at,
                    device: Some(matched_device),
                    inode: Some(matched_inode),
                    fingerprint: &fingerprint,
                    generation,
                })
                .await?;
            existing_entry = Some(entry);
        }
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
        let hierarchy = episode_hierarchy(&relative_path, &parsed);
        let series_identity = format!("series:{}:{}", root.id, hierarchy.series_path);
        let season_identity = format!("{series_identity}:season:{}", hierarchy.season_number);
        let episode_identity = Self::episode_identity_key(root, &hierarchy, &parsed);
        if let Some(existing_item_id) = existing_entry
            .as_ref()
            .and_then(|entry| entry.item_id.as_deref())
            && existing_entry
                .as_ref()
                .and_then(|entry| entry.item_identity_key.as_deref())
                != Some(episode_identity.as_str())
        {
            self.database
                .repair_episode_hierarchy_identities(
                    existing_item_id,
                    &series_identity,
                    &season_identity,
                    &episode_identity,
                )
                .await?;
        }
        let fingerprint_unchanged = existing_entry
            .as_ref()
            .is_some_and(|entry| entry.fingerprint.as_deref() == Some(fingerprint.as_slice()));
        let episode_is_current = fingerprint_unchanged
            && existing_entry
                .as_ref()
                .and_then(|entry| entry.item_identity_key.as_deref())
                == Some(episode_identity.as_str());
        let should_refresh_series_provider_ids = episode_is_current
            && !hierarchy.provider_ids.is_empty()
            && existing_entry.as_ref().is_some_and(|entry| {
                entry
                    .series_provider_ids_json
                    .as_deref()
                    .is_none_or(|value| value.is_empty() || value == "{}")
            })
            && refreshed_series
                .as_ref()
                .is_none_or(|series| !series.contains(&series_identity));
        if should_refresh_series_provider_ids {
            let series_provider_ids_json = provider_ids_json(&hierarchy.provider_ids);
            if let Some(series_provider_ids_json) = series_provider_ids_json.as_deref() {
                self.database
                    .update_local_provider_ids_for_identity_if_empty(
                        &series_identity,
                        series_provider_ids_json,
                    )
                    .await?;
            }
            if let Some(series) = refreshed_series.as_mut() {
                series.insert(series_identity.clone());
            }
        }
        if episode_is_current && let Some(existing_entry) = existing_entry.as_ref() {
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
            return Ok(ScanReport {
                discovered_files: 1,
                skipped_files: 1,
                ..ScanReport::default()
            });
        }

        let ensured = self
            .ensure_episode_hierarchy(library_id_text, root, &relative_path, &parsed, &hierarchy)
            .await?;
        if let Some(existing_entry) = existing_entry {
            let hierarchy_changed = self
                .database
                .reassign_media_source_item(&existing_entry.id, &ensured.episode_id)
                .await?;
            self.database
                .update_media_source_variant_labels(
                    &existing_entry.id,
                    parsed.edition_name.as_deref(),
                    parsed.quality_label.as_deref(),
                )
                .await?;
            if fingerprint_unchanged {
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
                return Ok(ScanReport {
                    discovered_files: 1,
                    created_items: ensured.created_items,
                    changed_files: usize::from(hierarchy_changed),
                    skipped_files: 1,
                    ..ScanReport::default()
                });
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
            return Ok(ScanReport {
                discovered_files: 1,
                created_items: ensured.created_items,
                changed_files: 1,
                ..ScanReport::default()
            });
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
                device,
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
                item_id: &ensured.episode_id,
                source_kind: if is_strm { "STRM_URL" } else { "LOCAL_FILE" },
                filesystem_entry_id: &entry_id,
                edition_name: parsed.edition_name.as_deref(),
                quality_label: parsed.quality_label.as_deref(),
                container: &container,
                size,
                external_url,
                strm_target_kind,
                is_default: ensured.episode_created,
            })
            .await?;
        Ok(ScanReport {
            discovered_files: 1,
            created_items: ensured.created_items,
            created_sources: 1,
            ..ScanReport::default()
        })
    }

    pub(super) async fn scan_unresolved_file(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        generation: &str,
    ) -> Result<ScanReport, ScannerError> {
        self.scan_plain_file(library_id_text, root, root_path, path, generation, false)
            .await
    }

    pub(super) async fn scan_home_video_file(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        generation: &str,
    ) -> Result<ScanReport, ScannerError> {
        self.scan_plain_file(library_id_text, root, root_path, path, generation, true)
            .await
    }

    async fn scan_plain_file(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        generation: &str,
        home_video: bool,
    ) -> Result<ScanReport, ScannerError> {
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
        if let Some(existing_entry) = existing_entry {
            if existing_entry.fingerprint.as_deref() == Some(fingerprint.as_slice()) {
                let expected_parent_identity =
                    movie_parent_folder_identity(&root.id, &relative_path);
                if existing_entry.parent_identity_key.as_deref()
                    != expected_parent_identity.as_deref()
                    && let Some(item_id) = existing_entry.item_id.as_deref()
                {
                    self.database
                        .repair_movie_parent_folder(
                            library_id_text,
                            &root.id,
                            &relative_path,
                            item_id,
                        )
                        .await?;
                }
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
                return Ok(ScanReport {
                    discovered_files: 1,
                    skipped_files: 1,
                    ..ScanReport::default()
                });
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
            return Ok(ScanReport {
                discovered_files: 1,
                changed_files: 1,
                ..ScanReport::default()
            });
        }
        let file_name = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(if home_video { "Video" } else { "Unresolved" });
        let title = clean_hierarchy_title(file_name);
        let title = if title.is_empty() {
            if home_video {
                "Video".to_owned()
            } else {
                "Unresolved".to_owned()
            }
        } else {
            title
        };
        let sort_title = title.to_lowercase();
        let identity_key = format!(
            "{}:{}:{}",
            if home_video { "video" } else { "unresolved" },
            root.id,
            relative_path
        );
        let item_id = ItemId::new().to_string();
        self.database
            .insert_hierarchy_item(NewHierarchyItem {
                id: &item_id,
                library_id: library_id_text,
                item_type: if home_video { "VIDEO" } else { "UNRESOLVED" },
                parent_id: None,
                series_id: None,
                season_number: None,
                episode_number: None,
                absolute_number: None,
                title: &title,
                sort_title: &sort_title,
                original_title: Some(&title),
                production_year: None,
                provider_ids_json: None,
                identification_status: if home_video {
                    "LOCAL_CONFIRMED"
                } else {
                    "PENDING"
                },
                identity_key: &identity_key,
            })
            .await?;
        let entry_id = FilesystemEntryId::new().to_string();
        self.database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: &entry_id,
                library_root_id: &root.id,
                relative_path: &relative_path,
                entry_kind: "FILE",
                size,
                modified_at,
                device,
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
                edition_name: None,
                quality_label: None,
                container: &container,
                size,
                external_url,
                strm_target_kind,
                is_default: true,
            })
            .await?;
        self.database
            .repair_movie_parent_folder(library_id_text, &root.id, &relative_path, &item_id)
            .await?;
        Ok(ScanReport {
            discovered_files: 1,
            created_items: 1,
            created_sources: 1,
            ..ScanReport::default()
        })
    }

    async fn ensure_hierarchy_item(
        &self,
        item: NewHierarchyItem<'_>,
        legacy_identity: Option<&str>,
    ) -> Result<(String, bool), ScannerError> {
        if let Some(existing) = self
            .database
            .find_media_item_by_identity(item.identity_key)
            .await?
        {
            self.database
                .update_unconfirmed_hierarchy_item(
                    &existing.id,
                    item.title,
                    item.sort_title,
                    item.original_title,
                    item.production_year,
                    item.provider_ids_json,
                )
                .await?;
            self.database.restore_media_item(&existing.id).await?;
            return Ok((existing.id, false));
        }
        if let Some(legacy_identity) = legacy_identity
            && let Some(existing) = self
                .database
                .find_media_item_by_identity(legacy_identity)
                .await?
            && self
                .database
                .adopt_media_item_identity(&existing.id, item.identity_key)
                .await?
        {
            self.database
                .update_unconfirmed_hierarchy_item(
                    &existing.id,
                    item.title,
                    item.sort_title,
                    item.original_title,
                    item.production_year,
                    item.provider_ids_json,
                )
                .await?;
            self.database.restore_media_item(&existing.id).await?;
            return Ok((existing.id, false));
        }
        let id = item.id.to_owned();
        self.database.insert_hierarchy_item(item).await?;
        Ok((id, true))
    }

    fn episode_identity_key(
        root: &StoredLibraryRoot,
        hierarchy: &EpisodeHierarchy,
        parsed: &ParsedEpisodeFilename,
    ) -> String {
        Self::episode_identity_key_for_root(&root.id, hierarchy, parsed)
    }

    fn episode_identity_key_for_root(
        root_id: &str,
        hierarchy: &EpisodeHierarchy,
        parsed: &ParsedEpisodeFilename,
    ) -> String {
        let edition_key = parsed
            .edition_name
            .as_deref()
            .unwrap_or("standard")
            .to_ascii_lowercase();
        format!(
            "episode:{}:{}:season:{}:episode:{}:edition:{}",
            root_id, hierarchy.series_path, hierarchy.season_number, parsed.episode, edition_key
        )
    }

    async fn ensure_episode_hierarchy(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        relative_path: &str,
        parsed: &ParsedEpisodeFilename,
        hierarchy: &EpisodeHierarchy,
    ) -> Result<EnsuredEpisodeHierarchy, ScannerError> {
        let series_sort_title = hierarchy.series_title.to_lowercase();
        let series_identity = format!("series:{}:{}", root.id, hierarchy.series_path);
        let legacy_series_identity = legacy_series_identity(root, hierarchy);
        let series_provider_ids_json = provider_ids_json(&hierarchy.provider_ids);
        let series_new_id = ItemId::new().to_string();
        let (series_id, series_created) = self
            .ensure_hierarchy_item(
                NewHierarchyItem {
                    id: &series_new_id,
                    library_id: library_id_text,
                    item_type: "SERIES",
                    parent_id: None,
                    series_id: None,
                    season_number: None,
                    episode_number: None,
                    absolute_number: None,
                    title: &hierarchy.series_title,
                    sort_title: &series_sort_title,
                    original_title: Some(&hierarchy.series_title),
                    production_year: hierarchy.production_year.map(i64::from),
                    provider_ids_json: series_provider_ids_json.as_deref(),
                    identification_status: "PENDING",
                    identity_key: &series_identity,
                },
                legacy_series_identity.as_deref(),
            )
            .await?;
        let season_title = if hierarchy.season_number == 0 {
            "Specials".to_owned()
        } else {
            format!("Season {:02}", hierarchy.season_number)
        };
        let season_identity = format!("{series_identity}:season:{}", hierarchy.season_number);
        let season_sort_title = season_title.to_lowercase();
        let season_new_id = ItemId::new().to_string();
        let legacy_season_identity = legacy_series_identity
            .as_deref()
            .map(|identity| format!("{identity}:season:{}", hierarchy.season_number));
        let (season_id, season_created) = self
            .ensure_hierarchy_item(
                NewHierarchyItem {
                    id: &season_new_id,
                    library_id: library_id_text,
                    item_type: "SEASON",
                    parent_id: Some(&series_id),
                    series_id: Some(&series_id),
                    season_number: Some(i64::from(hierarchy.season_number)),
                    episode_number: None,
                    absolute_number: None,
                    title: &season_title,
                    sort_title: &season_sort_title,
                    original_title: Some(&season_title),
                    production_year: None,
                    provider_ids_json: None,
                    identification_status: "PENDING",
                    identity_key: &season_identity,
                },
                legacy_season_identity.as_deref(),
            )
            .await?;
        let episode_identity = Self::episode_identity_key(root, hierarchy, parsed);
        let episode_title = parsed.title.clone();
        let episode_sort_title = episode_title.to_lowercase();
        let episode_new_id = ItemId::new().to_string();
        let legacy_episode_identity = format!("episode:{}:{}", root.id, relative_path);
        let (episode_id, episode_created) = self
            .ensure_hierarchy_item(
                NewHierarchyItem {
                    id: &episode_new_id,
                    library_id: library_id_text,
                    item_type: "EPISODE",
                    parent_id: Some(&season_id),
                    series_id: Some(&series_id),
                    season_number: Some(i64::from(hierarchy.season_number)),
                    episode_number: Some(i64::from(parsed.episode)),
                    absolute_number: parsed.absolute_number.map(i64::from),
                    title: &episode_title,
                    sort_title: &episode_sort_title,
                    original_title: Some(&episode_title),
                    production_year: None,
                    provider_ids_json: None,
                    identification_status: "PENDING",
                    identity_key: &episode_identity,
                },
                Some(&legacy_episode_identity),
            )
            .await?;
        Ok(EnsuredEpisodeHierarchy {
            episode_id,
            created_items: usize::from(series_created)
                + usize::from(season_created)
                + usize::from(episode_created),
            episode_created,
        })
    }

    pub async fn scan_movie_directory(
        &self,
        library_id: LibraryId,
        directory: &Path,
    ) -> Result<ScanReport, ScannerError> {
        let library_id_text = library_id.to_string();
        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_none()
        {
            return Err(ScannerError::LibraryNotFound);
        }

        let canonical_directory =
            fs::canonicalize(directory)
                .await
                .map_err(|source| ScannerError::Io {
                    path: directory.to_owned(),
                    source,
                })?;
        let roots = self.database.list_library_roots(&library_id_text).await?;
        let root = roots
            .into_iter()
            .filter(|root| canonical_directory.starts_with(&root.canonical_path))
            .max_by_key(|root| root.canonical_path.len())
            .ok_or_else(|| {
                ScannerError::InvalidRelativePath(format!(
                    "directory is outside library roots: {}",
                    canonical_directory.display()
                ))
            })?;
        if !root.is_available {
            return Ok(ScanReport {
                unavailable_roots: 1,
                ..ScanReport::default()
            });
        }

        let generation = Uuid::now_v7().to_string();
        let mut report = ScanReport::default();
        let mut walker = FileBatchWalker::new(&canonical_directory);
        while let Some(files) = walker.next_batch(FILE_BATCH_SIZE).await? {
            report.merge(
                self.scan_movie_file_batch(
                    &library_id_text,
                    &root,
                    Path::new(&root.canonical_path),
                    &files,
                    &generation,
                )
                .await?,
            );
        }
        Ok(report)
    }

    async fn scan_movie_file_batch(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        files: &[PathBuf],
        generation: &str,
    ) -> Result<ScanReport, ScannerError> {
        if files.is_empty() {
            return Ok(ScanReport::default());
        }
        let relative_paths = files
            .iter()
            .map(|path| {
                path.strip_prefix(root_path)
                    .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                    .to_str()
                    .map(str::to_owned)
                    .ok_or(ScannerError::NonUtf8Path)
            })
            .collect::<Result<Vec<_>, ScannerError>>()?;
        let existing_entries = self
            .database
            .list_filesystem_entries_for_paths(&root.id, &relative_paths)
            .await?;
        let quick_results = self
            .scan_movie_files_if_unchanged(
                &root.id,
                root_path,
                files,
                &existing_entries,
                self.scan_concurrency,
            )
            .await?;
        let mut report = ScanReport::default();
        let mut seen_entry_ids = Vec::with_capacity(files.len());
        let mut new_paths = Vec::new();
        let mut changed_paths = Vec::<(PathBuf, Option<String>)>::new();
        for (path, quick_result) in files.iter().zip(quick_results) {
            let inferred_variant_suffix = match quick_result {
                MovieQuickScanResult::Unchanged {
                    entry_id,
                    report: quick_report,
                } => {
                    seen_entry_ids.push(entry_id);
                    report.merge(quick_report);
                    continue;
                }
                MovieQuickScanResult::Rescan => None,
                MovieQuickScanResult::RescanWithVariantSuffix(suffix) => Some(suffix),
            };
            let relative_path = path
                .strip_prefix(root_path)
                .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                .to_str()
                .ok_or(ScannerError::NonUtf8Path)?;
            if !existing_entries.contains_key(relative_path) {
                new_paths.push(path.clone());
                continue;
            }
            changed_paths.push((path.clone(), inferred_variant_suffix));
        }
        for changed_report in self
            .scan_movie_files_with_concurrency(
                library_id_text,
                root,
                root_path,
                &changed_paths,
                generation,
                self.scan_concurrency,
            )
            .await?
        {
            report.merge(changed_report);
        }

        let mut pending_new_files = Vec::with_capacity(new_paths.len());
        for file in self
            .prepare_new_movie_files(root_path, &new_paths)
            .await?
            .into_iter()
            .flatten()
        {
            pending_new_files.push(file);
            if pending_new_files.len() == FILE_BATCH_SIZE {
                self.flush_new_movie_files(
                    library_id_text,
                    root,
                    generation,
                    &mut pending_new_files,
                    &mut report,
                )
                .await?;
            }
        }
        self.flush_new_movie_files(
            library_id_text,
            root,
            generation,
            &mut pending_new_files,
            &mut report,
        )
        .await?;
        self.database
            .mark_filesystem_entries_seen_batch(&seen_entry_ids, generation)
            .await?;
        Ok(report)
    }

    async fn scan_movie_files_with_concurrency(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        paths: &[(PathBuf, Option<String>)],
        generation: &str,
        configured_concurrency: usize,
    ) -> Result<Vec<ScanReport>, ScannerError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        let concurrency = configured_concurrency.max(1);
        // A regular movie scan can repair an old item's identity and reassign
        // one of its sources. Files with the same parsed movie identity must
        // therefore stay in input order; otherwise two source variants can
        // both observe the pre-repair rows and create duplicate logical items.
        // Different identities remain eligible for concurrent processing.
        let mut grouped_paths = Vec::<Vec<(usize, PathBuf, Option<String>)>>::new();
        let mut group_indexes = HashMap::<String, usize>::new();
        for (index, (path, cached_suffix)) in paths.iter().cloned().enumerate() {
            let inferred_suffix = match cached_suffix {
                Some(suffix) => Some(suffix),
                None => infer_sibling_movie_variant_suffix(&path).await,
            };
            let group_key = reconciliation_regular_group_key(
                &root.id,
                &path,
                MixedClassification::Movie,
                inferred_suffix.as_deref(),
                index,
            );
            let group_index = *group_indexes.entry(group_key).or_insert_with(|| {
                grouped_paths.push(Vec::new());
                grouped_paths.len() - 1
            });
            grouped_paths[group_index].push((index, path, inferred_suffix));
        }
        let mut tasks: JoinSet<ReconciliationRegularGroupTask> = JoinSet::new();
        let mut results = (0..paths.len()).map(|_| None).collect::<Vec<_>>();
        for group in grouped_paths {
            while tasks.len() >= concurrency {
                collect_reconciliation_regular_group_task(&mut tasks, &mut results).await?;
            }
            let scanner = self.clone();
            let library_id_text = library_id_text.to_owned();
            let root = root.clone();
            let root_path = root_path.to_owned();
            let generation = generation.to_owned();
            tasks.spawn(async move {
                let mut reports = Vec::with_capacity(group.len());
                for (index, path, inferred_suffix) in group {
                    let report = scanner
                        .scan_movie_file_with_inferred_suffix(
                            &library_id_text,
                            &root,
                            &root_path,
                            &path,
                            &generation,
                            inferred_suffix.as_deref(),
                        )
                        .await?;
                    reports.push((index, report));
                }
                Ok(reports)
            });
        }
        while !tasks.is_empty() {
            collect_reconciliation_regular_group_task(&mut tasks, &mut results).await?;
        }
        Ok(results.into_iter().flatten().collect())
    }

    async fn scan_movie_file_if_unchanged(
        &self,
        library_root_id: &str,
        root_path: &Path,
        path: &Path,
        existing_entry: &StoredFilesystemEntry,
    ) -> Result<MovieQuickScanResult, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(MovieQuickScanResult::Rescan);
        };
        if parse_movie_filename(file_name).is_none() || is_strm_file(path) {
            return Ok(MovieQuickScanResult::Rescan);
        }
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
        if !scan_fingerprint_matches_observation(
            existing_entry,
            &relative_path,
            size,
            modified_at,
            device,
            inode,
        ) {
            return Ok(MovieQuickScanResult::Rescan);
        }
        let expected_parent_identity =
            movie_parent_folder_identity(library_root_id, &relative_path);
        if existing_entry.parent_identity_key.as_deref() != expected_parent_identity.as_deref() {
            return Ok(MovieQuickScanResult::Rescan);
        }
        // CD-part and version-marker entries need the regular path to refresh their identity.
        if has_multi_part_marker(file_name) || has_source_variant_marker(file_name) {
            return Ok(MovieQuickScanResult::Rescan);
        }
        if let Some(inferred_variant_suffix) = infer_sibling_movie_variant_suffix(path).await {
            return Ok(MovieQuickScanResult::RescanWithVariantSuffix(
                inferred_variant_suffix,
            ));
        }
        Ok(MovieQuickScanResult::Unchanged {
            entry_id: existing_entry.id.clone(),
            report: ScanReport {
                discovered_files: 1,
                skipped_files: 1,
                ..ScanReport::default()
            },
        })
    }

    async fn scan_movie_files_if_unchanged(
        &self,
        library_root_id: &str,
        root_path: &Path,
        paths: &[PathBuf],
        existing_entries: &HashMap<String, StoredFilesystemEntry>,
        configured_concurrency: usize,
    ) -> Result<Vec<MovieQuickScanResult>, ScannerError> {
        let mut results = (0..paths.len())
            .map(|_| MovieQuickScanResult::Rescan)
            .collect::<Vec<_>>();
        let mut tasks: JoinSet<MovieFingerprintTask> = JoinSet::new();
        let concurrency = configured_concurrency.clamp(1, FINGERPRINT_CHECK_CONCURRENCY);
        for (index, path) in paths.iter().enumerate() {
            let relative_path = path
                .strip_prefix(root_path)
                .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                .to_str()
                .ok_or(ScannerError::NonUtf8Path)?;
            let Some(existing_entry) = existing_entries.get(relative_path).cloned() else {
                continue;
            };
            while tasks.len() >= concurrency {
                collect_movie_fingerprint_task(&mut tasks, &mut results).await?;
            }
            let scanner = self.clone();
            let root_path = root_path.to_owned();
            let path = path.clone();
            let library_root_id = library_root_id.to_owned();
            tasks.spawn(async move {
                let result = scanner
                    .scan_movie_file_if_unchanged(
                        &library_root_id,
                        &root_path,
                        &path,
                        &existing_entry,
                    )
                    .await?;
                Ok((index, result))
            });
        }
        while !tasks.is_empty() {
            collect_movie_fingerprint_task(&mut tasks, &mut results).await?;
        }
        Ok(results)
    }

    pub(super) async fn scan_file_if_fingerprint_unchanged(
        &self,
        root_path: &Path,
        path: &Path,
        existing_entry: &StoredFilesystemEntry,
    ) -> Result<Option<(String, ScanReport)>, ScannerError> {
        let observation = current_file_observation(root_path, path).await?;
        if !observation.matches(existing_entry) {
            return Ok(None);
        }
        Ok(Some((
            existing_entry.id.clone(),
            ScanReport {
                discovered_files: 1,
                skipped_files: 1,
                ..ScanReport::default()
            },
        )))
    }

    async fn scan_episode_file_if_unchanged(
        &self,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        existing_entries: &HashMap<String, StoredFilesystemEntry>,
    ) -> Result<Option<(String, ScanReport)>, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        if parse_episode_filename(file_name).is_none() {
            return Ok(None);
        }
        if is_strm_file(path) {
            return Ok(None);
        }
        let observation = current_file_observation(root_path, path).await?;
        let Some(existing_entry) = existing_entries.get(&observation.relative_path) else {
            return Ok(None);
        };
        let Some((entry_id, report, provider_update)) = self
            .scan_episode_file_if_unchanged_entry(root, path, &observation, existing_entry)
            .await?
        else {
            return Ok(None);
        };
        if let Some((series_identity, provider_ids_json)) = provider_update {
            self.database
                .update_local_provider_ids_for_identity_if_empty(
                    &series_identity,
                    &provider_ids_json,
                )
                .await?;
        }
        Ok(Some((entry_id, report)))
    }

    async fn scan_episode_file_if_unchanged_entry(
        &self,
        root: &StoredLibraryRoot,
        path: &Path,
        observation: &CurrentFileObservation,
        existing_entry: &StoredFilesystemEntry,
    ) -> Result<EpisodeQuickResult, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let Some(parsed) = parse_episode_filename(file_name) else {
            return Ok(None);
        };
        if is_strm_file(path) {
            return Ok(None);
        }
        if !observation.matches(existing_entry) {
            return Ok(None);
        }
        if existing_entry.item_id.is_none() {
            return Ok(None);
        }
        let hierarchy = episode_hierarchy(&observation.relative_path, &parsed);
        let series_identity = format!("series:{}:{}", root.id, hierarchy.series_path);
        let episode_identity = Self::episode_identity_key(root, &hierarchy, &parsed);
        if existing_entry.item_identity_key.as_deref() != Some(episode_identity.as_str()) {
            return Ok(None);
        }
        let series_provider_ids_missing = existing_entry
            .series_provider_ids_json
            .as_deref()
            .is_none_or(|value| value.is_empty() || value == "{}");
        let provider_update = if !hierarchy.provider_ids.is_empty() && series_provider_ids_missing {
            provider_ids_json(&hierarchy.provider_ids).map(|json| (series_identity, json))
        } else {
            None
        };
        Ok(Some((
            existing_entry.id.clone(),
            ScanReport {
                discovered_files: 1,
                skipped_files: 1,
                ..ScanReport::default()
            },
            provider_update,
        )))
    }

    async fn scan_episode_files_if_unchanged(
        &self,
        root: &StoredLibraryRoot,
        root_path: &Path,
        paths: &[PathBuf],
        existing_entries: &HashMap<String, StoredFilesystemEntry>,
        configured_concurrency: usize,
    ) -> Result<Vec<EpisodeQuickResult>, ScannerError> {
        let mut results = (0..paths.len()).map(|_| None).collect::<Vec<_>>();
        let mut tasks: JoinSet<EpisodeFingerprintTask> = JoinSet::new();
        let concurrency = configured_concurrency.clamp(1, FINGERPRINT_CHECK_CONCURRENCY);
        for (index, path) in paths.iter().enumerate() {
            let relative_path = path
                .strip_prefix(root_path)
                .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                .to_str()
                .ok_or(ScannerError::NonUtf8Path)?;
            let Some(existing_entry) = existing_entries.get(relative_path).cloned() else {
                continue;
            };
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| parse_episode_filename(name).is_none() || is_strm_file(path))
            {
                continue;
            }
            while tasks.len() >= concurrency {
                collect_episode_fingerprint_task(&mut tasks, &mut results).await?;
            }
            let scanner = self.clone();
            let root = root.clone();
            let root_path = root_path.to_owned();
            let path = path.clone();
            tasks.spawn(async move {
                let observation = current_file_observation(&root_path, &path).await?;
                let result = scanner
                    .scan_episode_file_if_unchanged_entry(
                        &root,
                        &path,
                        &observation,
                        &existing_entry,
                    )
                    .await?;
                Ok((index, result))
            });
        }
        while !tasks.is_empty() {
            collect_episode_fingerprint_task(&mut tasks, &mut results).await?;
        }
        Ok(results)
    }

    pub(super) async fn file_has_moved_entry(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
    ) -> Result<bool, ScannerError> {
        let metadata = fs::metadata(path)
            .await
            .map_err(|source| ScannerError::Io {
                path: path.to_owned(),
                source,
            })?;
        let (device, inode) = file_identity(&metadata);
        let (Some(device), Some(inode)) = (
            device.and_then(|value| i64::try_from(value).ok()),
            inode.and_then(|value| i64::try_from(value).ok()),
        ) else {
            return Ok(false);
        };
        let relative_path = path
            .strip_prefix(root_path)
            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
            .to_str()
            .ok_or(ScannerError::NonUtf8Path)?;
        Ok(self
            .database
            .find_filesystem_entry_by_inode(library_id_text, &root.id, device, inode, relative_path)
            .await?
            .is_some())
    }

    pub(super) async fn episode_path_has_legacy_identity(
        &self,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
    ) -> Result<bool, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(false);
        };
        let Some(parsed) = parse_episode_filename(file_name) else {
            return Ok(false);
        };
        let relative_path = path
            .strip_prefix(root_path)
            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
            .to_str()
            .ok_or(ScannerError::NonUtf8Path)?;
        let hierarchy = episode_hierarchy(relative_path, &parsed);
        let legacy_series = legacy_series_identity(root, &hierarchy);
        let legacy_season = legacy_series
            .as_deref()
            .map(|identity| format!("{identity}:season:{}", hierarchy.season_number));
        let legacy_episode = format!("episode:{}:{relative_path}", root.id);
        for identity in [legacy_series, legacy_season, Some(legacy_episode)] {
            if let Some(identity) = identity
                && self
                    .database
                    .find_media_item_by_identity(&identity)
                    .await?
                    .is_some()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn scan_unresolved_file_if_unchanged(
        &self,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        existing_entries: &HashMap<String, StoredFilesystemEntry>,
    ) -> Result<Option<(String, ScanReport)>, ScannerError> {
        if is_strm_file(path) {
            return Ok(None);
        }
        let observation = current_file_observation(root_path, path).await?;
        let Some(existing_entry) = existing_entries.get(&observation.relative_path) else {
            return Ok(None);
        };
        if !observation.matches(existing_entry) {
            return Ok(None);
        }
        let expected_parent_identity =
            movie_parent_folder_identity(&root.id, &observation.relative_path);
        if existing_entry.item_type.as_deref() == Some("MOVIE")
            && existing_entry.parent_identity_key.as_deref() != expected_parent_identity.as_deref()
        {
            return Ok(None);
        }
        Ok(Some((
            existing_entry.id.clone(),
            ScanReport {
                discovered_files: 1,
                skipped_files: 1,
                ..ScanReport::default()
            },
        )))
    }

    pub(super) async fn scan_sidecar_file(
        &self,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        existing_entries: &HashMap<String, StoredFilesystemEntry>,
        generation: &str,
    ) -> Result<(String, bool), ScannerError> {
        let observation = current_file_observation(root_path, path).await?;
        let relative_path = &observation.relative_path;
        let fingerprint = observation.fingerprint_v2();
        let device = observation
            .device
            .and_then(|value| i64::try_from(value).ok());
        let inode = observation
            .inode
            .and_then(|value| i64::try_from(value).ok());
        if let Some(existing_entry) = existing_entries.get(relative_path) {
            if observation.matches(existing_entry) {
                return Ok((existing_entry.id.clone(), false));
            }
            self.database
                .update_filesystem_entry(
                    &existing_entry.id,
                    observation.size,
                    observation.modified_at,
                    &fingerprint,
                    device,
                    inode,
                    generation,
                )
                .await?;
            return Ok((existing_entry.id.clone(), true));
        }
        let entry_id = FilesystemEntryId::new().to_string();
        self.database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: &entry_id,
                library_root_id: &root.id,
                relative_path: &relative_path,
                entry_kind: "FILE",
                size: observation.size,
                modified_at: observation.modified_at,
                device,
                inode,
                fingerprint: &fingerprint,
                last_seen_generation: generation,
            })
            .await?;
        Ok((entry_id, true))
    }
}

pub(super) fn is_supported_movie_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mkv" | "mp4" | "strm"
            )
        })
}

pub(super) fn is_supported_sidecar_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "nfo" | "jpg" | "jpeg" | "png" | "webp" | "edl" | "xml"
            )
        })
}

pub(super) fn is_strm_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"))
}

pub(super) async fn read_strm_target(path: &Path) -> Result<StrmTarget, ScannerError> {
    let file = fs::File::open(path)
        .await
        .map_err(|source| ScannerError::Io {
            path: path.to_owned(),
            source,
        })?;
    let mut contents = String::new();
    let bytes_read = file
        .take(u64::try_from(MAX_STRM_TARGET_BYTES.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_string(&mut contents)
        .await
        .map_err(|source| ScannerError::Io {
            path: path.to_owned(),
            source,
        })?;
    if bytes_read > MAX_STRM_TARGET_BYTES {
        return Err(ScannerError::Io {
            path: path.to_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "STRM target exceeds the size limit",
            ),
        });
    }
    Ok(classify_strm_target(&contents))
}

fn strm_target_kind_name(target: &StrmTarget) -> &'static str {
    match target.kind {
        StrmTargetKind::Empty => "EMPTY",
        StrmTargetKind::Url => "URL",
        StrmTargetKind::Path => "PATH",
        StrmTargetKind::Smb | StrmTargetKind::Ftp | StrmTargetKind::Unsupported => "OPAQUE",
    }
}

pub(super) fn is_file_change_only_sidecar(path: &Path) -> bool {
    is_external_subtitle_file(path)
        || is_media_info_sidecar_file(path)
        || path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("xml")
                    || extension.eq_ignore_ascii_case("nfo")
                    || matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "jpg" | "jpeg" | "png" | "webp" | "edl"
                    )
            })
}

pub(super) fn is_supported_incremental_sidecar_file(path: &Path) -> bool {
    is_supported_sidecar_file(path) || is_file_change_only_sidecar(path)
}

fn is_external_subtitle_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "srt" | "ass" | "ssa" | "vtt" | "sub" | "sup"
            )
        })
}

fn is_media_info_sidecar_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|file_name| file_name.to_str())
        .is_some_and(|file_name| file_name.to_ascii_lowercase().ends_with("-mediainfo.json"))
}
