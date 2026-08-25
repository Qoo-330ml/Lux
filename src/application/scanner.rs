use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use quick_xml::{events::Event, reader::Reader};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use tokio::{
    fs,
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};
use uuid::Uuid;

use crate::{
    application::{
        admin_events::{AdminEventHub, AdminEventScope, UserEventHub, UserEventScope},
        home::HomeService,
        library_covers::{AutoLibraryCoverResult, LibraryCoverService},
        media_matching::{MediaKind, clean_title, has_multi_part_marker, parse_media_name},
        metadata::MetadataEnricher,
        nfo::LocalNfoMetadataStore,
        people::PeopleService,
        probe::MediaProbeService,
        reidentify::{MetadataRefreshMode, MetadataReidentifyError, MetadataReidentifyService},
        strm_probe::StrmProbeService,
        strm_target::{StrmTarget, StrmTargetKind, classify_strm_target},
        thumbnails::ThumbnailService,
        watch::ChangeKind,
        webhooks::{WebhookEventType, WebhookService},
    },
    domain::ids::{FilesystemEntryId, ItemId, LibraryId, SourceId},
    observability::resources::ResourceMetrics,
    storage::{
        Database, FilesystemEntryMove, NewFilesystemEntry, NewHierarchyItem, NewMediaItem,
        NewMediaSource, NewMovieFile, NewScanJobEvent, StorageError,
        StoredEpisodeIdentityCandidate, StoredFilesystemEntry, StoredLibraryRoot,
        StoredReconciliationScanEntry, StoredScanJob, StoredScanJobPath,
    },
};

const FILE_BATCH_SIZE: usize = 500;
pub const BACKGROUND_SCAN_BATCH_SIZE: usize = 100;
const DISCOVERY_BATCH_SIZE: usize = 16;
const MOVIE_PREPARATION_CONCURRENCY: usize = 8;

#[derive(Clone)]
pub struct LibraryScanner {
    database: Database,
}

impl LibraryScanner {
    pub fn new(database: Database) -> Self {
        Self { database }
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
            let mut seen_entry_ids = Vec::with_capacity(FILE_BATCH_SIZE);
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
                let mut pending_new_files = Vec::with_capacity(FILE_BATCH_SIZE);
                let mut new_paths = Vec::new();
                for path in files {
                    if let Some((entry_id, quick_report)) = self
                        .scan_movie_file_if_unchanged(
                            &library_id_text,
                            &root.id,
                            &root_path,
                            &path,
                            &existing_entries,
                        )
                        .await?
                    {
                        seen_entry_ids.push(entry_id);
                        report.merge(quick_report);
                    } else {
                        let relative_path = path
                            .strip_prefix(&root_path)
                            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                            .to_str()
                            .ok_or(ScannerError::NonUtf8Path)?;
                        if !existing_entries.contains_key(relative_path) {
                            new_paths.push(path);
                        } else {
                            report.merge(
                                self.scan_movie_file(
                                    &library_id_text,
                                    &root,
                                    &root_path,
                                    &path,
                                    &generation,
                                )
                                .await?,
                            );
                        }
                    }
                }
                for file in self
                    .prepare_new_movie_files(&root_path, &new_paths)
                    .await?
                    .into_iter()
                    .flatten()
                {
                    pending_new_files.push(file);
                    if pending_new_files.len() == FILE_BATCH_SIZE {
                        self.flush_new_movie_files(
                            &library_id_text,
                            &root,
                            &generation,
                            &mut pending_new_files,
                            &mut report,
                        )
                        .await?;
                    }
                }
                self.flush_new_movie_files(
                    &library_id_text,
                    &root,
                    &generation,
                    &mut pending_new_files,
                    &mut report,
                )
                .await?;
                self.database
                    .mark_filesystem_entries_seen_batch(&seen_entry_ids, &generation)
                    .await?;
                seen_entry_ids.clear();
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
                for path in files {
                    if let Some((entry_id, quick_report)) = self
                        .scan_episode_file_if_unchanged(&root, &root_path, &path, &existing_entries)
                        .await?
                    {
                        seen_entry_ids.push(entry_id);
                        report.merge(quick_report);
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
                for path in files {
                    let classification = classify_mixed_file(&root_path, &path).await;
                    let quick_report = match classification {
                        MixedClassification::Movie => {
                            self.scan_movie_file_if_unchanged(
                                &library_id_text,
                                &root.id,
                                &root_path,
                                &path,
                                &existing_entries,
                            )
                            .await?
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
                                &library_id_text,
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
                    let result = match classification {
                        MixedClassification::Movie => {
                            self.scan_movie_file(
                                &library_id_text,
                                &root,
                                &root_path,
                                &path,
                                &generation,
                            )
                            .await?
                        }
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

    async fn scan_episode_file(
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
        let fingerprint =
            compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
        let existing_entry = self
            .database
            .find_filesystem_entry(&root.id, &relative_path)
            .await?;
        let mut existing_entry = existing_entry;
        let inode = inode.and_then(|value| i64::try_from(value).ok());
        if existing_entry.is_none()
            && let Some(inode) = inode
            && let Some(entry) = self
                .database
                .find_filesystem_entry_by_inode(library_id_text, &root.id, inode, &relative_path)
                .await?
        {
            self.database
                .move_filesystem_entry(FilesystemEntryMove {
                    entry_id: &entry.id,
                    library_root_id: &root.id,
                    relative_path: &relative_path,
                    size,
                    modified_at,
                    inode: Some(inode),
                    fingerprint: &fingerprint,
                    generation,
                })
                .await?;
            existing_entry = Some(entry);
        }
        let hierarchy = episode_hierarchy(&relative_path, &parsed);
        let series_identity = format!("series:{}:{}", root.id, hierarchy.series_path);
        let season_identity = format!("{series_identity}:season:{}", hierarchy.season_number);
        let episode_identity = Self::episode_identity_key(root, &hierarchy, &parsed);
        let current_identity_item = if existing_entry.is_some() {
            self.database
                .find_media_item_by_identity(&episode_identity)
                .await?
        } else {
            None
        };
        if let Some(existing_item_id) = existing_entry
            .as_ref()
            .and_then(|entry| entry.item_id.as_deref())
            && current_identity_item
                .as_ref()
                .is_none_or(|item| item.id.as_str() != existing_item_id)
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
        let episode_is_current = if fingerprint_unchanged {
            current_identity_item.as_ref().is_some_and(|item| {
                existing_entry
                    .as_ref()
                    .and_then(|entry| entry.item_id.as_deref())
                    == Some(item.id.as_str())
            })
        } else {
            false
        };
        let should_refresh_series_provider_ids = episode_is_current
            && !hierarchy.provider_ids.is_empty()
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
                .mark_filesystem_entry_seen(&existing_entry.id, generation)
                .await?;
            self.database
                .update_filesystem_entry_inode(&existing_entry.id, inode)
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
                    .mark_filesystem_entry_seen(&existing_entry.id, generation)
                    .await?;
                self.database
                    .update_filesystem_entry_inode(&existing_entry.id, inode)
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
                    generation,
                )
                .await?;
            self.database
                .update_filesystem_entry_inode(&existing_entry.id, inode)
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

    async fn scan_unresolved_file(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        generation: &str,
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
        let fingerprint =
            compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
        if let Some(existing_entry) = self
            .database
            .find_filesystem_entry(&root.id, &relative_path)
            .await?
        {
            if existing_entry.fingerprint.as_deref() == Some(fingerprint.as_slice()) {
                if let Some(item_id) = existing_entry.item_id.as_deref() {
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
                    .mark_filesystem_entry_seen(&existing_entry.id, generation)
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
            .unwrap_or("Unresolved");
        let title = clean_hierarchy_title(file_name);
        let title = if title.is_empty() {
            "Unresolved".to_owned()
        } else {
            title
        };
        let sort_title = title.to_lowercase();
        let identity_key = format!("unresolved:{}:{}", root.id, relative_path);
        let item_id = ItemId::new().to_string();
        self.database
            .insert_hierarchy_item(NewHierarchyItem {
                id: &item_id,
                library_id: library_id_text,
                item_type: "UNRESOLVED",
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
                identification_status: "PENDING",
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
                inode: inode.and_then(|value| i64::try_from(value).ok()),
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
        let edition_key = parsed
            .edition_name
            .as_deref()
            .unwrap_or("standard")
            .to_ascii_lowercase();
        format!(
            "episode:{}:{}:season:{}:episode:{}:edition:{}",
            root.id, hierarchy.series_path, hierarchy.season_number, parsed.episode, edition_key
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
            for path in files {
                report.merge(
                    self.scan_movie_file(
                        &library_id_text,
                        &root,
                        Path::new(&root.canonical_path),
                        &path,
                        &generation,
                    )
                    .await?,
                );
            }
        }
        Ok(report)
    }

    async fn scan_movie_file_if_unchanged(
        &self,
        library_id_text: &str,
        library_root_id: &str,
        root_path: &Path,
        path: &Path,
        existing_entries: &HashMap<String, StoredFilesystemEntry>,
    ) -> Result<Option<(String, ScanReport)>, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        if parse_movie_filename(file_name).is_none() || is_strm_file(path) {
            return Ok(None);
        }
        let relative_path = path
            .strip_prefix(root_path)
            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
            .to_str()
            .ok_or(ScannerError::NonUtf8Path)?
            .to_owned();
        let Some(existing_entry) = existing_entries.get(&relative_path) else {
            return Ok(None);
        };
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
            compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
        if existing_entry.fingerprint.as_deref() != Some(fingerprint.as_slice()) {
            return Ok(None);
        }
        if let Some(item_id) = existing_entry.item_id.as_deref() {
            self.database
                .repair_movie_parent_folder(
                    library_id_text,
                    library_root_id,
                    &relative_path,
                    item_id,
                )
                .await?;
        }
        if has_multi_part_marker(file_name) {
            let Some(parsed_name) = parse_movie_filename(file_name) else {
                return Ok(None);
            };
            let target_item = self
                .database
                .find_media_item(
                    library_id_text,
                    &parsed_name.sort_title,
                    parsed_name.production_year.map(i64::from),
                )
                .await?;
            if target_item
                .as_ref()
                .is_none_or(|item| existing_entry.item_id.as_deref() != Some(item.id.as_str()))
            {
                return Ok(None);
            }
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
        let Some(parsed) = parse_episode_filename(file_name) else {
            return Ok(None);
        };
        if is_strm_file(path) {
            return Ok(None);
        }
        let (relative_path, fingerprint) = current_file_fingerprint(root_path, path).await?;
        let Some(existing_entry) = existing_entries.get(&relative_path) else {
            return Ok(None);
        };
        if existing_entry.fingerprint.as_deref() != Some(fingerprint.as_slice()) {
            return Ok(None);
        }
        let Some(item_id) = existing_entry.item_id.as_deref() else {
            return Ok(None);
        };
        let hierarchy = episode_hierarchy(&relative_path, &parsed);
        let series_identity = format!("series:{}:{}", root.id, hierarchy.series_path);
        let season_identity = format!("{series_identity}:season:{}", hierarchy.season_number);
        let episode_identity = Self::episode_identity_key(root, &hierarchy, &parsed);
        let Some(current_identity_item) = self
            .database
            .find_media_item_by_identity(&episode_identity)
            .await?
        else {
            return Ok(None);
        };
        if current_identity_item.id != item_id {
            return Ok(None);
        }
        self.database
            .repair_episode_hierarchy_identities(
                item_id,
                &series_identity,
                &season_identity,
                &episode_identity,
            )
            .await?;
        if !hierarchy.provider_ids.is_empty() {
            let provider_ids_json = provider_ids_json(&hierarchy.provider_ids);
            if let Some(provider_ids_json) = provider_ids_json.as_deref() {
                self.database
                    .update_local_provider_ids_for_identity_if_empty(
                        &series_identity,
                        provider_ids_json,
                    )
                    .await?;
            }
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

    async fn scan_unresolved_file_if_unchanged(
        &self,
        library_id_text: &str,
        root: &StoredLibraryRoot,
        root_path: &Path,
        path: &Path,
        existing_entries: &HashMap<String, StoredFilesystemEntry>,
    ) -> Result<Option<(String, ScanReport)>, ScannerError> {
        if is_strm_file(path) {
            return Ok(None);
        }
        let (relative_path, fingerprint) = current_file_fingerprint(root_path, path).await?;
        let Some(existing_entry) = existing_entries.get(&relative_path) else {
            return Ok(None);
        };
        if existing_entry.fingerprint.as_deref() != Some(fingerprint.as_slice()) {
            return Ok(None);
        }
        if let Some(item_id) = existing_entry.item_id.as_deref() {
            self.database
                .repair_movie_parent_folder(library_id_text, &root.id, &relative_path, item_id)
                .await?;
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

    async fn prepare_new_movie_file(
        &self,
        root_path: &Path,
        path: &Path,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        self.prepare_new_movie_file_with_folder_provider_ids(root_path, path, None)
            .await
    }

    async fn prepare_new_movie_file_with_folder_provider_ids(
        &self,
        root_path: &Path,
        path: &Path,
        folder_provider_ids: Option<&BTreeMap<String, String>>,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let Some(parsed_name) = parse_movie_filename(file_name) else {
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
        let fingerprint =
            compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
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

    async fn prepare_new_movie_files(
        &self,
        root_path: &Path,
        paths: &[PathBuf],
    ) -> Result<Vec<Option<NewMovieFile>>, ScannerError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
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
            if tasks.len() >= MOVIE_PREPARATION_CONCURRENCY {
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

    async fn flush_new_movie_files(
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
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(ScanReport::default());
        };
        let Some(parsed_name) = parse_movie_filename(file_name) else {
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
        let fingerprint =
            compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
        let mut report = ScanReport {
            discovered_files: 1,
            ..ScanReport::default()
        };
        let existing_entry = self
            .database
            .find_filesystem_entry(&root.id, &relative_path)
            .await?;
        if let Some(existing_entry) = existing_entry.as_ref()
            && let Some(item_id) = existing_entry.item_id.as_deref()
        {
            self.database
                .repair_movie_parent_folder(library_id_text, &root.id, &relative_path, item_id)
                .await?;
        }
        if !has_multi_part_marker(file_name)
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
                    .mark_filesystem_entry_seen(&existing_entry.id, generation)
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

        let existing_item = self
            .database
            .find_media_item(
                library_id_text,
                &parsed_name.sort_title,
                parsed_name.production_year.map(i64::from),
            )
            .await?;
        let (item_id, created_item) = if let Some(item) = existing_item {
            (item.id, false)
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
                    .mark_filesystem_entry_seen(&existing_entry.id, generation)
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
                inode: inode.and_then(|value| i64::try_from(value).ok()),
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
}

#[derive(Clone)]
pub struct ScanJobService {
    scanner: LibraryScanner,
    database: Database,
    admin_events: AdminEventHub,
    user_events: UserEventHub,
    scan_lock: Arc<Semaphore>,
    library_covers: Option<LibraryCoverService>,
    strm_probe: Option<StrmProbeService>,
    people: Option<PeopleService>,
    local_nfo: Option<LocalNfoMetadataStore>,
    home: Option<HomeService>,
    webhooks: Option<WebhookService>,
    resources: ResourceMetrics,
    cancellation_flags: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncrementalScanChange {
    pub root_id: String,
    pub relative_path: String,
    pub kind: ChangeKind,
}

impl ScanJobService {
    pub fn new(database: Database) -> Self {
        Self {
            scanner: LibraryScanner::new(database.clone()),
            database,
            admin_events: AdminEventHub::new(),
            user_events: UserEventHub::new(),
            scan_lock: Arc::new(Semaphore::new(1)),
            library_covers: None,
            strm_probe: None,
            people: None,
            local_nfo: None,
            home: None,
            webhooks: None,
            resources: ResourceMetrics::new(),
            cancellation_flags: Arc::new(Mutex::new(HashMap::new())),
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

    fn cancellation_flag(&self, job_id: &str) -> Arc<AtomicBool> {
        let mut flags = match self.cancellation_flags.lock() {
            Ok(flags) => flags,
            Err(poisoned) => poisoned.into_inner(),
        };
        flags
            .entry(job_id.to_owned())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    }

    fn clear_cancellation_flag(&self, job_id: &str) {
        let mut flags = match self.cancellation_flags.lock() {
            Ok(flags) => flags,
            Err(poisoned) => poisoned.into_inner(),
        };
        flags.remove(job_id);
    }

    async fn cancel_running_job(&self, job_id: &str) -> Result<ScanBatchReport, ScanJobError> {
        self.database
            .clear_reconciliation_scan_entries(job_id)
            .await?;
        self.database.clear_scan_job_paths(job_id).await?;
        self.database
            .finish_scan_job(job_id, "CANCELLED", None)
            .await?;
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

    async fn cancellation_requested(
        &self,
        job_id: &str,
        job_cancel_requested: bool,
        flag: &AtomicBool,
    ) -> Result<bool, ScanJobError> {
        if job_cancel_requested || flag.load(Ordering::Acquire) {
            flag.store(true, Ordering::Release);
            return Ok(true);
        }
        if self.database.scan_job_cancel_requested(job_id).await? {
            flag.store(true, Ordering::Release);
            return Ok(true);
        }
        Ok(false)
    }

    async fn update_activity(
        &self,
        job_id: &str,
        current_item: Option<&str>,
        scan_phase: &str,
    ) -> Result<(), ScanJobError> {
        let current_item = current_item.and_then(safe_scan_activity_label);
        self.database
            .update_scan_job_activity(job_id, current_item.as_deref(), scan_phase)
            .await?;
        self.admin_events.publish(AdminEventScope::Jobs);
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
            .get_or_create_incremental_scan_job(&library_id_text, true)
            .await?;
        self.cancellation_flag(&job.id);
        for (root_id, relative_path, kind) in valid_changes {
            self.database
                .enqueue_incremental_scan_path(
                    &job.id,
                    &root_id,
                    &relative_path,
                    change_kind_name(kind),
                )
                .await?;
        }
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
        if let Some(active) = self
            .database
            .find_active_scan_job_for_library(library_id)
            .await?
        {
            if active.job_type != "INCREMENTAL_SCAN" {
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
                    .find_active_scan_job_for_library(library_id)
                    .await?
            {
                return Err(ScanJobError::AlreadyActive(active.id));
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
        let root_ids = roots.into_iter().map(|root| root.id).collect::<Vec<_>>();
        if let Err(error) = self
            .database
            .create_reconciliation_scan_job(
                &id,
                &library_id_text,
                &generation,
                &root_ids,
                auto_metadata_match,
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
        let _scan_permit = self.acquire_scan_lock().await?;
        let report = self.run_batch_unlocked(job_id, batch_size).await?;
        if report.processed > 0
            && let Some(home) = &self.home
        {
            home.invalidate();
            self.user_events.publish(UserEventScope::Home);
        }
        Ok(report)
    }

    async fn run_batch_unlocked(
        &self,
        job_id: &str,
        batch_size: usize,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Err(ScanJobError::JobNotFound);
        };
        let cancellation = self.cancellation_flag(job_id);
        if job.cancel_requested {
            cancellation.store(true, Ordering::Release);
        }
        if job.scan_phase == "POSTPROCESSING" {
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

        if !job.discovery_completed {
            return self
                .run_reconciliation_discovery_batch(&job, batch_size, &cancellation)
                .await;
        }
        self.run_reconciliation_file_batch(&job, batch_size, &cancellation)
            .await
    }

    async fn run_reconciliation_discovery_batch(
        &self,
        job: &StoredScanJob,
        batch_size: usize,
        cancellation: &AtomicBool,
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
        let mut unavailable_root_ids = HashSet::new();
        let mut discovered_count = job.total_count;
        for directory in directories {
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(&job.id).await;
            }
            if unavailable_root_ids.contains(&directory.library_root_id) {
                continue;
            }
            let Some(root) = self
                .database
                .find_library_root(&directory.library_root_id)
                .await?
            else {
                self.database
                    .discard_reconciliation_root_entries(&job.id, &directory.library_root_id)
                    .await?;
                continue;
            };
            match discover_reconciliation_directory(&root, &directory.relative_path, cancellation)
                .await
            {
                Ok(Some(discovered)) => {
                    discovered_count = discovered_count.saturating_add(
                        i64::try_from(discovered.media_files.len()).unwrap_or(i64::MAX),
                    );
                    if !root.is_available {
                        self.database
                            .update_library_root_availability(&root.id, true)
                            .await?;
                    }
                    self.database
                        .complete_reconciliation_directory(
                            &job.id,
                            &root.id,
                            &directory.relative_path,
                            &discovered.directories,
                            &discovered.media_files,
                        )
                        .await?;
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
            self.database
                .update_scan_job_discovery_progress(&job.id, discovered_count)
                .await?;
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
        Ok(ScanBatchReport {
            status: "RUNNING".to_owned(),
            processed: 0,
            created_items: 0,
            completed: false,
        })
    }

    async fn run_reconciliation_file_batch(
        &self,
        job: &StoredScanJob,
        batch_size: usize,
        cancellation: &AtomicBool,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let roots = self.database.list_library_roots(&job.library_id).await?;
        let library = self.database.find_library(&job.library_id).await?;
        let library_kind = library
            .as_ref()
            .map_or("MOVIE", |library| library.kind.as_str());
        let scan_concurrency = library
            .as_ref()
            .map_or(2, |library| library.scan_concurrency);
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
                "FINALIZING"
            } else {
                "INDEXING"
            },
        )
        .await?;
        if batch.is_empty() {
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
                removed_count = removed_count.saturating_add(
                    usize::try_from(
                        self.database
                            .mark_missing_filesystem_entries(&root.id, &job.generation)
                            .await?,
                    )
                    .unwrap_or(usize::MAX),
                );
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
            if removed_count > 0 {
                self.publish_webhook_event_with_data(
                    job,
                    WebhookEventType::MediaRemoved,
                    None,
                    json!({ "removedCount": removed_count }),
                )
                .await;
            }
            self.publish_webhook_event(job, WebhookEventType::ScanCompleted, None)
                .await;
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
        let mut quick_seen_entry_ids = Vec::<String>::new();
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
        for entry in &batch {
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
                self.database
                    .mark_filesystem_entry_missing_by_path(&root.id, &entry.relative_path)
                    .await?;
                next_count = next_count.saturating_add(1);
                processed = processed.saturating_add(1);
                completed_entries.push(entry.clone());
                continue;
            }

            let existing_entries = existing_entries_by_root
                .get(&root.id)
                .ok_or_else(|| ScannerError::LibraryNotFound)?;
            let quick_result = match library_kind {
                "SERIES" => {
                    self.scanner
                        .scan_episode_file_if_unchanged(
                            root,
                            Path::new(&root.canonical_path),
                            &path,
                            existing_entries,
                        )
                        .await?
                }
                "MIXED" => {
                    match classify_mixed_file(Path::new(&root.canonical_path), &path).await {
                        MixedClassification::Movie => {
                            self.scanner
                                .scan_movie_file_if_unchanged(
                                    &job.library_id,
                                    &root.id,
                                    Path::new(&root.canonical_path),
                                    &path,
                                    existing_entries,
                                )
                                .await?
                        }
                        MixedClassification::Episode => {
                            self.scanner
                                .scan_episode_file_if_unchanged(
                                    root,
                                    Path::new(&root.canonical_path),
                                    &path,
                                    existing_entries,
                                )
                                .await?
                        }
                        MixedClassification::Unresolved => {
                            self.scanner
                                .scan_unresolved_file_if_unchanged(
                                    &job.library_id,
                                    root,
                                    Path::new(&root.canonical_path),
                                    &path,
                                    existing_entries,
                                )
                                .await?
                        }
                    }
                }
                _ => None,
            };
            if let Some((entry_id, quick_report)) = quick_result {
                quick_seen_entry_ids.push(entry_id);
                next_count = next_count.saturating_add(1);
                processed = processed.saturating_add(1);
                created_items = created_items.saturating_add(quick_report.created_items);
                completed_entries.push(entry.clone());
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

            let result = match library_kind {
                "MOVIE" => {
                    self.scanner
                        .scan_movie_file(
                            &job.library_id,
                            root,
                            Path::new(&root.canonical_path),
                            &path,
                            &job.generation,
                        )
                        .await
                }
                "SERIES" => {
                    self.scanner
                        .scan_episode_file(
                            &job.library_id,
                            root,
                            Path::new(&root.canonical_path),
                            &path,
                            &job.generation,
                        )
                        .await
                }
                "MIXED" => {
                    match classify_mixed_file(Path::new(&root.canonical_path), &path).await {
                        MixedClassification::Movie => {
                            self.scanner
                                .scan_movie_file(
                                    &job.library_id,
                                    root,
                                    Path::new(&root.canonical_path),
                                    &path,
                                    &job.generation,
                                )
                                .await
                        }
                        MixedClassification::Episode => {
                            self.scanner
                                .scan_episode_file(
                                    &job.library_id,
                                    root,
                                    Path::new(&root.canonical_path),
                                    &path,
                                    &job.generation,
                                )
                                .await
                        }
                        MixedClassification::Unresolved => {
                            self.scanner
                                .scan_unresolved_file(
                                    &job.library_id,
                                    root,
                                    Path::new(&root.canonical_path),
                                    &path,
                                    &job.generation,
                                )
                                .await
                        }
                    }
                }
                _ => Err(ScannerError::LibraryNotFound),
            };
            let report = match result {
                Ok(report) => report,
                Err(error) => {
                    return self
                        .fail_reconciliation_job(job, error, &completed_entries, next_count)
                        .await;
                }
            };
            created_items = created_items.saturating_add(report.created_items);
            next_count = next_count.saturating_add(1);
            processed = processed.saturating_add(1);
            completed_entries.push(entry.clone());
        }
        self.database
            .mark_filesystem_entries_seen_batch(&quick_seen_entry_ids, &job.generation)
            .await?;
        for (root_id, paths) in new_paths_by_root {
            self.database
                .record_scan_job_targets(&job.id, &root_id, &paths, "NEW")
                .await?;
        }
        for (root_id, paths) in changed_paths_by_root {
            self.database
                .record_scan_job_targets(&job.id, &root_id, &paths, "CHANGED")
                .await?;
        }
        self.finish_reconciliation_file_batch(
            job,
            completed_entries,
            processed,
            created_items,
            next_count,
            None,
        )
        .await
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
        let mut quick_seen_entry_ids = Vec::<String>::new();
        let mut new_files = Vec::<(
            usize,
            String,
            PathBuf,
            PathBuf,
            StoredReconciliationScanEntry,
        )>::new();

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
                self.database
                    .mark_filesystem_entry_missing_by_path(&root.id, &entry.relative_path)
                    .await?;
                next_count = next_count.saturating_add(1);
                processed = processed.saturating_add(1);
                completed_entries.push((index, entry.clone()));
                continue;
            }

            if existing_entries_by_root
                .get(&root.id)
                .and_then(|entries| entries.get(&entry.relative_path))
                .is_some()
            {
                let existing_entries = existing_entries_by_root
                    .get(&root.id)
                    .ok_or_else(|| ScannerError::LibraryNotFound)?;
                if let Some((entry_id, quick_report)) = self
                    .scanner
                    .scan_movie_file_if_unchanged(
                        &job.library_id,
                        &root.id,
                        Path::new(&root.canonical_path),
                        &path,
                        existing_entries,
                    )
                    .await?
                {
                    quick_seen_entry_ids.push(entry_id);
                    next_count = next_count.saturating_add(1);
                    processed = processed.saturating_add(1);
                    completed_entries.push((index, entry.clone()));
                    created_items = created_items.saturating_add(quick_report.created_items);
                    continue;
                }
                changed_paths_by_root
                    .entry(root.id.clone())
                    .or_default()
                    .push(entry.relative_path.clone());
                if let Err(error) = self
                    .scanner
                    .scan_movie_file(
                        &job.library_id,
                        root,
                        Path::new(&root.canonical_path),
                        &path,
                        &job.generation,
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
                next_count = next_count.saturating_add(1);
                processed = processed.saturating_add(1);
                completed_entries.push((index, entry.clone()));
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

        let concurrency = self
            .effective_scan_concurrency(configured_concurrency)
            .await;
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

        for root in roots {
            let Some(files) = prepared_files.get(&root.id) else {
                continue;
            };
            let inserted = match self
                .database
                .insert_movie_files_batch(&job.library_id, &root.id, &job.generation, files)
                .await
            {
                Ok(inserted) => inserted,
                Err(error) => {
                    let completed = completed_entries
                        .iter()
                        .map(|(_, entry)| entry.clone())
                        .collect::<Vec<_>>();
                    return self
                        .fail_reconciliation_job(job, error.into(), &completed, next_count)
                        .await;
                }
            };
            created_items = created_items.saturating_add(inserted);
        }

        self.database
            .mark_filesystem_entries_seen_batch(&quick_seen_entry_ids, &job.generation)
            .await?;
        for (root_id, paths) in new_paths_by_root {
            self.database
                .record_scan_job_targets(&job.id, &root_id, &paths, "NEW")
                .await?;
        }
        for (root_id, paths) in changed_paths_by_root {
            self.database
                .record_scan_job_targets(&job.id, &root_id, &paths, "CHANGED")
                .await?;
        }

        completed_entries.sort_by_key(|(index, _)| *index);
        let completed_entries = completed_entries
            .into_iter()
            .map(|(_, entry)| entry)
            .collect();
        self.finish_reconciliation_file_batch(
            job,
            completed_entries,
            processed,
            created_items,
            next_count,
            Some(concurrency),
        )
        .await
    }

    async fn finish_reconciliation_file_batch(
        &self,
        job: &StoredScanJob,
        completed_entries: Vec<StoredReconciliationScanEntry>,
        processed: usize,
        created_items: usize,
        next_count: i64,
        effective_concurrency: Option<usize>,
    ) -> Result<ScanBatchReport, ScanJobError> {
        self.database
            .complete_reconciliation_files(&job.id, &completed_entries, next_count)
            .await?;
        let batch_details = match effective_concurrency {
            Some(concurrency) => format!(
                r#"{{"processed":{processed},"total":{next_count},"concurrency":{concurrency}}}"#
            ),
            None => format!(r#"{{"processed":{processed},"total":{next_count}}}"#),
        };
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

    async fn fail_reconciliation_job(
        &self,
        job: &StoredScanJob,
        error: ScannerError,
        completed_entries: &[StoredReconciliationScanEntry],
        next_count: i64,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let error_code = error.code();
        if !completed_entries.is_empty() {
            self.database
                .complete_reconciliation_files(&job.id, completed_entries, next_count)
                .await?;
        }
        self.database
            .finish_scan_job(&job.id, "FAILED", Some(&error.to_string()))
            .await?;
        self.record_event(&job.id, "ERROR", error_code, "扫描任务失败", "{}")
            .await;
        self.publish_webhook_event(job, WebhookEventType::ScanFailed, Some(error_code))
            .await;
        self.clear_cancellation_flag(&job.id);
        Err(error.into())
    }

    async fn run_incremental_batch(
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
                self.publish_webhook_event(&job, WebhookEventType::ScanCompleted, None)
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
        let mut created_items = 0_usize;
        for path in &paths {
            if cancellation.load(Ordering::Acquire) {
                return self.cancel_running_job(job_id).await;
            }
            let created = match self
                .process_incremental_path(&library.kind, &job, path, cancellation)
                .await
            {
                Ok(created) => created,
                Err(error) => {
                    self.database
                        .finish_scan_job(job_id, "FAILED", Some(&error.to_string()))
                        .await?;
                    self.record_event(job_id, "ERROR", error.code(), "局部扫描任务失败", "{}")
                        .await;
                    self.publish_webhook_event(
                        &job,
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

    async fn publish_webhook_event(
        &self,
        job: &StoredScanJob,
        event_type: WebhookEventType,
        error_code: Option<&str>,
    ) {
        self.publish_webhook_event_with_data(job, event_type, error_code, json!({}))
            .await;
    }

    async fn publish_media_added_event(&self, job: &StoredScanJob, added_count: usize) {
        if added_count > 0 {
            self.publish_webhook_event_with_data(
                job,
                WebhookEventType::MediaAdded,
                None,
                json!({ "addedCount": added_count }),
            )
            .await;
        }
    }

    async fn publish_webhook_event_with_data(
        &self,
        job: &StoredScanJob,
        event_type: WebhookEventType,
        error_code: Option<&str>,
        extra: Value,
    ) {
        let Some(webhooks) = self.webhooks.as_ref() else {
            return;
        };
        let occurred_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or(0);
        let dedupe_key = format!("scan:{}:{}", job.id, event_type.as_str());
        let mut data = json!({
            "jobId": job.id,
            "libraryId": job.library_id,
            "jobType": job.job_type,
            "status": job.status,
            "processedCount": job.processed_count,
            "totalCount": job.total_count,
            "errorCode": error_code,
        });
        if let (Value::Object(data), Value::Object(extra)) = (&mut data, extra) {
            data.extend(extra);
        }
        let result = webhooks
            .publish(event_type, &dedupe_key, occurred_at, data)
            .await;
        if result.is_err() {
            tracing::warn!(job_id = %job.id, event_type = event_type.as_str(), "failed to enqueue webhook event");
        }
    }

    async fn process_incremental_path(
        &self,
        library_kind: &str,
        job: &StoredScanJob,
        path: &StoredScanJobPath,
        cancellation: &AtomicBool,
    ) -> Result<usize, ScannerError> {
        let root = self
            .database
            .find_library_root(&path.library_root_id)
            .await?
            .ok_or(ScannerError::LibraryNotFound)?;
        let root_path = Path::new(&root.canonical_path);
        let media_path = root_path.join(&path.relative_path);
        if path.change_kind == "REMOVE" || fs::metadata(&media_path).await.is_err() {
            self.database
                .mark_filesystem_entry_missing_by_path(&root.id, &path.relative_path)
                .await?;
            return Ok(0);
        }
        let metadata = fs::metadata(&media_path)
            .await
            .map_err(|source| ScannerError::Io {
                path: media_path.clone(),
                source,
            })?;
        if metadata.is_dir() {
            let mut created_items = 0_usize;
            let mut walker = FileBatchWalker::new(&media_path);
            while let Some(files) = walker.next_batch(FILE_BATCH_SIZE).await? {
                for file in files {
                    if cancellation.load(Ordering::Acquire) {
                        return Ok(created_items);
                    }
                    created_items = created_items.saturating_add(
                        self.process_incremental_file(library_kind, job, &root, root_path, &file)
                            .await?,
                    );
                }
            }
            Ok(created_items)
        } else if is_supported_movie_file(&media_path) {
            if cancellation.load(Ordering::Acquire) {
                return Ok(0);
            }
            self.process_incremental_file(library_kind, job, &root, root_path, &media_path)
                .await
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
    ) -> Result<usize, ScannerError> {
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
            "MIXED" => match classify_mixed_file(root_path, file).await {
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
        Ok(report.created_items)
    }

    pub async fn run_to_completion(
        &self,
        job_id: &str,
        batch_size: usize,
        probe: Option<MediaProbeService>,
    ) -> Result<(), ScanJobError> {
        self.run_to_completion_with_metadata_and_thumbnails(job_id, batch_size, probe, None, None)
            .await
    }

    pub async fn run_to_completion_with_metadata(
        &self,
        job_id: &str,
        batch_size: usize,
        probe: Option<MediaProbeService>,
        metadata: Option<MetadataReidentifyService>,
    ) -> Result<(), ScanJobError> {
        self.run_to_completion_with_metadata_and_thumbnails(
            job_id, batch_size, probe, metadata, None,
        )
        .await
    }

    pub async fn run_to_completion_with_metadata_and_thumbnails(
        &self,
        job_id: &str,
        batch_size: usize,
        probe: Option<MediaProbeService>,
        metadata: Option<MetadataReidentifyService>,
        thumbnails: Option<ThumbnailService>,
    ) -> Result<(), ScanJobError> {
        if batch_size == 0 {
            return Err(ScanJobError::InvalidBatchSize);
        }
        let _scan_permit = self.acquire_scan_lock().await?;
        let mut created_items = 0_usize;
        loop {
            let report = self.run_batch_unlocked(job_id, batch_size).await?;
            if report.processed > 0
                && let Some(home) = &self.home
            {
                home.invalidate();
                self.user_events.publish(UserEventScope::Home);
            }
            created_items = created_items.saturating_add(report.created_items);
            if !report.completed {
                tokio::task::yield_now().await;
                continue;
            }
            if report.status == "COMPLETED" {
                let completed_job = self
                    .database
                    .find_scan_job(job_id)
                    .await?
                    .ok_or(ScanJobError::JobNotFound)?;
                let incremental = completed_job.job_type == "INCREMENTAL_SCAN";
                drop(_scan_permit);
                if incremental {
                    self.run_metadata_after_incremental_scan(job_id).await?;
                    self.run_thumbnails_after_incremental_scan(job_id, thumbnails)
                        .await?;
                    if completed_job.auto_metadata_match {
                        if let Some(metadata) = metadata {
                            self.schedule_online_metadata_after_incremental_scan(job_id, metadata)
                                .await;
                        }
                        if let Some(strm_probe) = self.strm_probe.clone() {
                            self.schedule_strm_probe_after_incremental_scan(
                                job_id,
                                &completed_job.library_id,
                                strm_probe,
                            )
                            .await;
                        }
                    }
                    self.run_auto_library_cover_after_scan(job_id).await?;
                    if let Some(home) = &self.home {
                        home.invalidate();
                        self.user_events.publish(UserEventScope::Home);
                    }
                    self.publish_media_added_event(&completed_job, created_items)
                        .await;
                    return Ok(());
                }
                self.database.retry_failed_scan_job_targets(job_id).await?;
                self.run_probe_after_scan(job_id, probe).await?;
                self.run_metadata_after_scan(job_id).await?;
                self.run_thumbnails_after_scan(job_id, thumbnails).await?;
                self.database
                    .clear_completed_scan_job_targets(job_id)
                    .await?;
                self.database
                    .complete_scan_job_postprocessing(job_id)
                    .await?;
                self.run_auto_library_cover_after_scan(job_id).await?;
                if completed_job.auto_metadata_match {
                    if let Some(metadata) = metadata {
                        self.schedule_online_metadata_after_scan(job_id, metadata)
                            .await;
                    }
                }
                if let Some(home) = &self.home {
                    home.invalidate();
                    self.user_events.publish(UserEventScope::Home);
                }
                self.publish_media_added_event(&completed_job, created_items)
                    .await;
            }
            return Ok(());
        }
    }

    async fn run_auto_library_cover_after_scan(&self, job_id: &str) -> Result<(), ScanJobError> {
        let Some(covers) = self.library_covers.as_ref() else {
            return Ok(());
        };
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Err(ScanJobError::JobNotFound);
        };
        let Ok(library_id) = job.library_id.parse::<LibraryId>() else {
            tracing::warn!(
                job_id,
                library_id = %job.library_id,
                "automatic library cover generation skipped for invalid library ID"
            );
            return Ok(());
        };
        match covers.generate_if_eligible(library_id).await {
            Ok(AutoLibraryCoverResult::Generated) => {
                self.record_event(
                    job_id,
                    "INFO",
                    "LIBRARY_COVER_GENERATED",
                    "已自动生成媒体库封面",
                    "{}",
                )
                .await;
            }
            Ok(
                AutoLibraryCoverResult::BelowThreshold
                | AutoLibraryCoverResult::ExistingCover
                | AutoLibraryCoverResult::TaskNotRegistered
                | AutoLibraryCoverResult::AlreadyHandled,
            ) => {}
            Err(error) => {
                tracing::warn!(job_id, %error, "automatic library cover generation failed");
                self.record_event(
                    job_id,
                    "ERROR",
                    "LIBRARY_COVER_FAILED",
                    "自动媒体库封面生成失败",
                    "{}",
                )
                .await;
            }
        }
        Ok(())
    }

    async fn acquire_scan_lock(&self) -> Result<OwnedSemaphorePermit, ScanJobError> {
        Arc::clone(&self.scan_lock)
            .acquire_owned()
            .await
            .map_err(|_| ScanJobError::ScanLockClosed)
    }

    async fn effective_scan_concurrency(&self, configured: i64) -> usize {
        self.resources
            .background_concurrency(usize::try_from(configured).unwrap_or(1))
            .await
    }

    async fn run_thumbnails_after_scan(
        &self,
        job_id: &str,
        thumbnails: Option<ThumbnailService>,
    ) -> Result<(), ScanJobError> {
        if self.database.find_scan_job(job_id).await?.is_none() {
            return Err(ScanJobError::JobNotFound);
        }
        let Some(thumbnails) = thumbnails else {
            self.database
                .skip_pending_scan_job_target_stage(job_id, "ITEM", "THUMBNAIL")
                .await?;
            return Ok(());
        };
        match thumbnails.generate_scan_job(job_id).await {
            Ok(report) if report.failed == 0 => {
                let details = format!(
                    r#"{{"considered":{},"generated":{},"reused":{},"failed":{},"skippedStrm":{}}}"#,
                    report.considered,
                    report.generated,
                    report.reused,
                    report.failed,
                    report.skipped_strm,
                );
                self.record_event(
                    job_id,
                    "INFO",
                    "THUMBNAIL_COMPLETED",
                    "视频缩略图任务完成",
                    &details,
                )
                .await;
            }
            Ok(report) => {
                let details = format!(
                    r#"{{"considered":{},"generated":{},"reused":{},"failed":{},"skippedStrm":{}}}"#,
                    report.considered,
                    report.generated,
                    report.reused,
                    report.failed,
                    report.skipped_strm,
                );
                self.record_event(
                    job_id,
                    "WARN",
                    "THUMBNAIL_FAILED",
                    "部分视频缩略图生成失败",
                    &details,
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(job_id, %error, "scan thumbnail task failed");
                self.record_event(
                    job_id,
                    "ERROR",
                    "THUMBNAIL_FAILED",
                    "视频缩略图任务失败",
                    "{}",
                )
                .await;
            }
        }
        Ok(())
    }

    async fn schedule_online_metadata_after_scan(
        &self,
        scan_job_id: &str,
        metadata: MetadataReidentifyService,
    ) {
        let Some(scan_job) = self
            .database
            .find_scan_job(scan_job_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let Some(library) = self
            .database
            .find_library(&scan_job.library_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        if library.scraper_id.as_deref().is_none() {
            return;
        }
        let Ok(library_id) = scan_job.library_id.parse::<LibraryId>() else {
            tracing::warn!(
                scan_job_id,
                library_id = %scan_job.library_id,
                "automatic metadata matching skipped for invalid library ID"
            );
            return;
        };
        let job = match metadata
            .create_library_refresh_job(&library_id.to_string(), MetadataRefreshMode::FillMissing)
            .await
        {
            Ok(job) => job,
            Err(MetadataReidentifyError::InvalidItemCount) => return,
            Err(_) => {
                tracing::warn!(
                    scan_job_id,
                    "scan completed but automatic metadata matching could not be queued"
                );
                self.record_event(
                    scan_job_id,
                    "ERROR",
                    "METADATA_AUTO_MATCH_QUEUE_FAILED",
                    "自动元数据匹配任务创建失败",
                    "{}",
                )
                .await;
                return;
            }
        };
        let job_id = job.id.clone();
        tokio::spawn(async move {
            metadata.run(&job_id).await;
        });
        let details = format!(
            r#"{{"itemCount":{},"jobId":"{}","mode":"FILL_MISSING"}}"#,
            job.total_count, job.id
        );
        self.record_event(
            scan_job_id,
            "INFO",
            "METADATA_AUTO_MATCH_QUEUED",
            "已提交自动元数据匹配任务",
            &details,
        )
        .await;
    }

    async fn schedule_online_metadata_after_incremental_scan(
        &self,
        scan_job_id: &str,
        metadata: MetadataReidentifyService,
    ) {
        let Some(scan_job) = self
            .database
            .find_scan_job(scan_job_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        let Some(library) = self
            .database
            .find_library(&scan_job.library_id)
            .await
            .ok()
            .flatten()
        else {
            return;
        };
        if !library.realtime_metadata_auto_match_enabled || library.scraper_id.as_deref().is_none()
        {
            return;
        }
        let Ok(item_ids) = self
            .database
            .list_media_item_ids_for_incremental_scan(scan_job_id)
            .await
        else {
            tracing::warn!(
                scan_job_id,
                "incremental scan completed but affected media items could not be found"
            );
            return;
        };
        for item_ids in item_ids.chunks(100) {
            let job = match metadata.create_fill_missing_job(item_ids.to_vec()).await {
                Ok(job) => job,
                Err(_) => {
                    tracing::warn!(
                        scan_job_id,
                        item_count = item_ids.len(),
                        "incremental scan completed but automatic metadata matching could not be queued"
                    );
                    self.record_event(
                        scan_job_id,
                        "ERROR",
                        "METADATA_AUTO_MATCH_QUEUE_FAILED",
                        "自动元数据匹配任务创建失败",
                        "{}",
                    )
                    .await;
                    continue;
                }
            };
            let job_id = job.id.clone();
            let worker = metadata.clone();
            tokio::spawn(async move {
                worker.run(&job_id).await;
            });
            let details = format!(
                r#"{{"itemCount":{},"jobId":"{}","mode":"FILL_MISSING"}}"#,
                job.total_count, job.id
            );
            self.record_event(
                scan_job_id,
                "INFO",
                "METADATA_AUTO_MATCH_QUEUED",
                "已提交自动元数据匹配任务",
                &details,
            )
            .await;
        }
    }

    async fn schedule_strm_probe_after_incremental_scan(
        &self,
        scan_job_id: &str,
        library_id: &str,
        strm_probe: StrmProbeService,
    ) {
        let Ok(library_id) = library_id.parse::<LibraryId>() else {
            tracing::warn!(
                scan_job_id,
                library_id,
                "incremental scan completed but automatic STRM probe skipped for invalid library ID"
            );
            return;
        };
        let job = match strm_probe
            .create_configured_incremental_job(scan_job_id, library_id)
            .await
        {
            Ok(Some(job)) => job,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(
                    scan_job_id,
                    %error,
                    "incremental scan completed but automatic STRM probe could not be queued"
                );
                return;
            }
        };
        let job_id = job.id.clone();
        let worker = strm_probe;
        tokio::spawn(async move {
            if let Err(error) = worker.run(&job_id).await {
                tracing::error!(job_id = %job_id, %error, "automatic STRM probe stopped");
            }
        });
        self.record_event(
            scan_job_id,
            "INFO",
            "STRM_MEDIA_INFO_AUTO_QUEUED",
            "已提交新增 STRM 媒体信息识别任务",
            &format!(
                r#"{{"jobId":"{}","itemCount":{}}}"#,
                job.id, job.total_count
            ),
        )
        .await;
    }

    async fn run_metadata_after_scan(&self, job_id: &str) -> Result<(), ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Err(ScanJobError::JobNotFound);
        };
        if self.database.find_library(&job.library_id).await?.is_none() {
            return Err(ScanJobError::LibraryNotFound);
        }
        let enricher = MetadataEnricher::new(self.database.clone());
        let enricher = match self.people.clone() {
            Some(people) => enricher.with_people(people),
            None => enricher,
        };
        let enricher = match self.local_nfo.clone() {
            Some(local_nfo) => enricher.with_nfo_store(local_nfo),
            None => enricher,
        };
        let result = enricher.enrich_scan_job(job_id).await;
        match result {
            Ok(report) => {
                let details = format!(
                    r#"{{"nfoLoaded":{},"nfoFailed":{},"nfoSkipped":{},"imagesFound":{}}}"#,
                    report.nfo_loaded, report.nfo_failed, report.nfo_skipped, report.images_found,
                );
                self.record_event(
                    job_id,
                    "INFO",
                    "METADATA_COMPLETED",
                    "本地元数据处理完成",
                    &details,
                )
                .await;
            }
            Err(_) => {
                tracing::warn!(
                    job_id,
                    "scan completed but local metadata enrichment failed"
                );
                self.record_event(
                    job_id,
                    "ERROR",
                    "METADATA_FAILED",
                    "本地元数据处理失败",
                    "{}",
                )
                .await;
            }
        }
        Ok(())
    }

    async fn run_metadata_after_incremental_scan(&self, job_id: &str) -> Result<(), ScanJobError> {
        let enricher = MetadataEnricher::new(self.database.clone());
        let enricher = match self.people.clone() {
            Some(people) => enricher.with_people(people),
            None => enricher,
        };
        let enricher = match self.local_nfo.clone() {
            Some(local_nfo) => enricher.with_nfo_store(local_nfo),
            None => enricher,
        };
        match enricher.enrich_incremental_scan(job_id).await {
            Ok(report) => {
                let details = format!(
                    r#"{{"nfoLoaded":{},"nfoFailed":{},"nfoSkipped":{},"imagesFound":{}}}"#,
                    report.nfo_loaded, report.nfo_failed, report.nfo_skipped, report.images_found,
                );
                self.record_event(
                    job_id,
                    "INFO",
                    "METADATA_COMPLETED",
                    "局部扫描本地元数据处理完成",
                    &details,
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(
                    job_id,
                    %error,
                    "incremental scan local metadata enrichment failed"
                );
                self.record_event(
                    job_id,
                    "ERROR",
                    "METADATA_FAILED",
                    "局部扫描本地元数据处理失败",
                    "{}",
                )
                .await;
            }
        }
        Ok(())
    }

    async fn run_thumbnails_after_incremental_scan(
        &self,
        job_id: &str,
        thumbnails: Option<ThumbnailService>,
    ) -> Result<(), ScanJobError> {
        let Some(thumbnails) = thumbnails else {
            return Ok(());
        };
        match thumbnails.generate_incremental_scan(job_id).await {
            Ok(report) if report.failed == 0 => {
                let details = format!(
                    r#"{{"considered":{},"generated":{},"reused":{},"failed":{},"skippedStrm":{}}}"#,
                    report.considered,
                    report.generated,
                    report.reused,
                    report.failed,
                    report.skipped_strm,
                );
                self.record_event(
                    job_id,
                    "INFO",
                    "THUMBNAIL_COMPLETED",
                    "局部扫描视频缩略图任务完成",
                    &details,
                )
                .await;
            }
            Ok(report) => {
                let details = format!(
                    r#"{{"considered":{},"generated":{},"reused":{},"failed":{},"skippedStrm":{}}}"#,
                    report.considered,
                    report.generated,
                    report.reused,
                    report.failed,
                    report.skipped_strm,
                );
                self.record_event(
                    job_id,
                    "WARN",
                    "THUMBNAIL_FAILED",
                    "局部扫描视频缩略图任务部分失败",
                    &details,
                )
                .await;
            }
            Err(error) => {
                tracing::warn!(job_id, %error, "incremental scan thumbnail task failed");
                self.record_event(
                    job_id,
                    "WARN",
                    "THUMBNAIL_FAILED",
                    "局部扫描视频缩略图任务失败",
                    "{}",
                )
                .await;
            }
        }
        Ok(())
    }

    async fn run_probe_after_scan(
        &self,
        job_id: &str,
        probe: Option<MediaProbeService>,
    ) -> Result<(), ScanJobError> {
        if self.database.find_scan_job(job_id).await?.is_none() {
            return Err(ScanJobError::JobNotFound);
        }
        let Some(probe) = probe else {
            self.database
                .skip_pending_scan_job_target_stage(job_id, "SOURCE", "PROBE")
                .await?;
            return Ok(());
        };
        let job = self
            .database
            .find_scan_job(job_id)
            .await?
            .ok_or(ScanJobError::JobNotFound)?;
        match probe.probe_scan_job(job_id, &job.library_id).await {
            Ok(report) => {
                let details = format!(
                    r#"{{"attempted":{},"ready":{},"failed":{},"timedOut":{},"skipped":{}}}"#,
                    report.attempted, report.ready, report.failed, report.timed_out, report.skipped,
                );
                self.record_event(job_id, "INFO", "PROBE_COMPLETED", "媒体探测完成", &details)
                    .await;
            }
            Err(error) => {
                tracing::warn!(job_id, %error, "scan completed but media probe failed");
                self.record_event(job_id, "ERROR", "PROBE_FAILED", "媒体探测任务失败", "{}")
                    .await;
            }
        }
        Ok(())
    }

    pub async fn active_job_ids(&self) -> Result<Vec<String>, ScanJobError> {
        let mut ids = Vec::new();
        for status in ["PENDING", "RUNNING"] {
            ids.extend(
                self.database
                    .list_scan_jobs(Some(status), 0, 10_000)
                    .await?
                    .into_iter()
                    .map(|job| job.id),
            );
        }
        ids.sort();
        ids.dedup();
        Ok(ids)
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
        self.record_event(job_id, "INFO", "CANCEL_REQUESTED", "已请求取消任务", "{}")
            .await;
        Ok(())
    }

    pub async fn retry(&self, job_id: &str) -> Result<ScanJob, ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Err(ScanJobError::JobNotFound);
        };
        if !matches!(job.status.as_str(), "FAILED" | "CANCELLED") {
            return Err(ScanJobError::AlreadyActive(job.id));
        }
        let Ok(library_id) = job.library_id.parse::<LibraryId>() else {
            return Err(ScanJobError::LibraryNotFound);
        };
        if job.status == "CANCELLED" {
            return self
                .create_movie_scan_job_with_metadata(library_id, job.auto_metadata_match)
                .await;
        }
        if let Some(active) = self
            .database
            .find_active_scan_job(&job.library_id, &job.job_type)
            .await?
        {
            return Err(ScanJobError::AlreadyActive(active.id));
        }
        if job.job_type == "RECONCILE_LIBRARY"
            && !self
                .database
                .has_reconciliation_scan_entries(&job.id)
                .await?
        {
            return self
                .create_movie_scan_job_with_metadata(library_id, job.auto_metadata_match)
                .await;
        }
        if !self.database.retry_scan_job(&job.id).await? {
            return Err(ScanJobError::AlreadyActive(job.id));
        }
        self.get_job(&job.id).await
    }

    async fn get_job(&self, id: &str) -> Result<ScanJob, ScanJobError> {
        self.database
            .find_scan_job(id)
            .await?
            .map(scan_job)
            .ok_or(ScanJobError::JobNotFound)
    }

    async fn record_event(
        &self,
        job_id: &str,
        level: &str,
        event_code: &str,
        message: &str,
        details_json: &str,
    ) {
        let id = Uuid::now_v7().to_string();
        let _ = self
            .database
            .append_scan_job_event(NewScanJobEvent {
                id: &id,
                job_id,
                level,
                event_code,
                message,
                details_json,
            })
            .await;
        self.admin_events.publish(AdminEventScope::Jobs);
    }
}

type MoviePreparationOutput = (
    usize,
    String,
    StoredReconciliationScanEntry,
    Option<NewMovieFile>,
);
type MoviePreparationTask = Result<MoviePreparationOutput, ScannerError>;

async fn join_movie_preparation(
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

async fn collect_movie_preparation_task(
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
    pub finished_at: Option<i64>,
    pub current_item: Option<String>,
    pub scan_phase: String,
}

fn scan_job(job: StoredScanJob) -> ScanJob {
    ScanJob {
        id: job.id,
        library_id: job.library_id,
        job_type: job.job_type,
        status: job.status,
        generation: job.generation,
        cursor: job.cursor,
        processed_count: job.processed_count,
        total_count: job.total_count,
        discovery_completed: job.discovery_completed,
        cancel_requested: job.cancel_requested,
        error: job.error,
        finished_at: job.finished_at,
        current_item: job.current_item,
        scan_phase: job.scan_phase,
    }
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
    Scanner(ScannerError),
    Storage(StorageError),
}

impl std::fmt::Display for ScanJobError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::ItemNotFound => formatter.write_str("media item not found"),
            Self::JobNotFound => formatter.write_str("scan job not found"),
            Self::NoChanges => formatter.write_str("incremental scan has no valid changes"),
            Self::AlreadyActive(id) => write!(formatter, "scan job already active: {id}"),
            Self::InvalidBatchSize => formatter.write_str("scan batch size must be positive"),
            Self::ScanLockClosed => formatter.write_str("scan lock is closed"),
            Self::Scanner(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

fn normalize_incremental_path(value: &str) -> Result<String, ScanJobError> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ScanJobError::Scanner(ScannerError::InvalidRelativePath(
            value.to_owned(),
        )));
    }
    Ok(value.to_owned())
}

fn media_source_folder(value: &str) -> Result<String, ScanJobError> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(ScanJobError::Scanner(ScannerError::InvalidRelativePath(
            value.to_owned(),
        )));
    }
    let folder = path
        .parent()
        .and_then(|parent| parent.to_str())
        .unwrap_or("");
    if folder.is_empty() {
        Ok(".".to_owned())
    } else {
        Ok(folder.to_owned())
    }
}

fn change_kind_name(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Create => "CREATE",
        ChangeKind::Modify => "MODIFY",
        ChangeKind::Rename => "RENAME",
        ChangeKind::Remove => "REMOVE",
    }
}

impl std::error::Error for ScanJobError {}

impl From<ScannerError> for ScanJobError {
    fn from(error: ScannerError) -> Self {
        Self::Scanner(error)
    }
}

impl From<StorageError> for ScanJobError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanReport {
    pub discovered_files: usize,
    pub created_items: usize,
    pub created_sources: usize,
    pub changed_files: usize,
    pub marked_missing: usize,
    pub unavailable_roots: usize,
    pub skipped_files: usize,
}

impl ScanReport {
    fn merge(&mut self, other: Self) {
        self.discovered_files += other.discovered_files;
        self.created_items += other.created_items;
        self.created_sources += other.created_sources;
        self.changed_files += other.changed_files;
        self.marked_missing += other.marked_missing;
        self.unavailable_roots += other.unavailable_roots;
        self.skipped_files += other.skipped_files;
    }
}

pub fn compute_file_fingerprint(
    relative_path: &str,
    size: i64,
    modified_at: i64,
    device: Option<u64>,
    inode: Option<u64>,
) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"LUX-FP-1\0");
    hasher.update((relative_path.len() as u64).to_le_bytes());
    hasher.update(relative_path.as_bytes());
    hasher.update(size.to_le_bytes());
    hasher.update(modified_at.to_le_bytes());
    hasher.update(device.unwrap_or_default().to_le_bytes());
    hasher.update(inode.unwrap_or_default().to_le_bytes());
    hasher.finalize().to_vec()
}

async fn current_file_fingerprint(
    root_path: &Path,
    path: &Path,
) -> Result<(String, Vec<u8>), ScannerError> {
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
    let fingerprint = compute_file_fingerprint(&relative_path, size, modified_at, device, inode);
    Ok((relative_path, fingerprint))
}

fn file_identity(metadata: &std::fs::Metadata) -> (Option<u64>, Option<u64>) {
    #[cfg(unix)]
    {
        (Some(metadata.dev()), Some(metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        (None, None)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedMovieFilename {
    pub title: String,
    pub sort_title: String,
    pub production_year: Option<i32>,
    pub edition_name: Option<String>,
    pub quality_label: Option<String>,
    pub provider_ids: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedEpisodeFilename {
    pub title: String,
    pub sort_title: String,
    pub production_year: Option<i32>,
    pub season: u32,
    pub episode: u32,
    pub absolute_number: Option<u32>,
    pub edition_name: Option<String>,
    pub quality_label: Option<String>,
    pub provider_ids: BTreeMap<String, String>,
}

#[derive(Clone, Copy)]
enum MixedClassification {
    Movie,
    Episode,
    Unresolved,
}

async fn classify_mixed_file(root: &Path, path: &Path) -> MixedClassification {
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return MixedClassification::Unresolved;
    };
    if parse_episode_filename(file_name).is_some() {
        return MixedClassification::Episode;
    }
    let series_nfo = path
        .strip_prefix(root)
        .ok()
        .and_then(|relative| relative.components().next())
        .map(|first| root.join(first.as_os_str()).join("tvshow.nfo"));
    if let Some(series_nfo) = series_nfo
        && nfo_root_is(&series_nfo, "tvshow").await
    {
        return MixedClassification::Unresolved;
    }
    let movie_nfo = path
        .parent()
        .map(|directory| directory.join("movie.nfo"))
        .filter(|candidate| candidate.exists())
        .or_else(|| {
            let candidate = path.with_extension("nfo");
            candidate.exists().then_some(candidate)
        });
    if let Some(movie_nfo) = movie_nfo
        && nfo_root_is(&movie_nfo, "movie").await
    {
        return MixedClassification::Movie;
    }
    if parse_movie_filename(file_name).is_some_and(|parsed| parsed.production_year.is_some()) {
        MixedClassification::Movie
    } else {
        MixedClassification::Unresolved
    }
}

async fn nfo_root_is(path: &Path, expected: &str) -> bool {
    let Ok(bytes) = fs::read(path).await else {
        return false;
    };
    let mut reader = Reader::from_reader(bytes.as_slice());
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event)) => {
                return event
                    .name()
                    .as_ref()
                    .eq_ignore_ascii_case(expected.as_bytes());
            }
            Ok(Event::Eof) | Err(_) => return false,
            Ok(_) => buffer.clear(),
        }
    }
}

pub fn parse_movie_filename(filename: &str) -> Option<ParsedMovieFilename> {
    parse_media_name(filename, MediaKind::Movie).map(|parsed| ParsedMovieFilename {
        title: parsed.title,
        sort_title: parsed.sort_title,
        production_year: parsed.production_year,
        edition_name: parsed.edition_name,
        quality_label: parsed.quality_label,
        provider_ids: parsed.provider_ids,
    })
}

fn movie_provider_ids(
    path: &Path,
    file_provider_ids: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut provider_ids = file_provider_ids.clone();
    for (provider, provider_id) in movie_folder_provider_ids(path) {
        provider_ids.entry(provider).or_insert(provider_id);
    }
    provider_ids
}

fn movie_folder_provider_ids(path: &Path) -> BTreeMap<String, String> {
    let Some(folder_name) = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
    else {
        return BTreeMap::new();
    };
    parse_media_name(folder_name, MediaKind::Movie)
        .map(|folder| folder.provider_ids)
        .unwrap_or_default()
}

fn provider_ids_json(provider_ids: &BTreeMap<String, String>) -> Option<String> {
    (!provider_ids.is_empty())
        .then(|| serde_json::to_string(provider_ids).unwrap_or_else(|_| "{}".to_owned()))
}

pub fn parse_episode_filename(filename: &str) -> Option<ParsedEpisodeFilename> {
    let parsed = parse_media_name(filename, MediaKind::Episode)?;
    let season = parsed.season?;
    let episode = parsed.episode?;
    let title = if parsed.title.is_empty() {
        format!("Episode {episode:02}")
    } else {
        parsed.title
    };
    Some(ParsedEpisodeFilename {
        title,
        sort_title: parsed.sort_title,
        production_year: parsed.production_year,
        season,
        episode,
        absolute_number: parsed.absolute_number,
        edition_name: parsed.edition_name,
        quality_label: parsed.quality_label,
        provider_ids: parsed.provider_ids,
    })
}

fn clean_hierarchy_title(value: &str) -> String {
    clean_title(value)
}

#[derive(Debug, Eq, PartialEq)]
struct EpisodeHierarchy {
    series_path: String,
    series_title: String,
    production_year: Option<i32>,
    provider_ids: BTreeMap<String, String>,
    season_number: u32,
}

struct EnsuredEpisodeHierarchy {
    episode_id: String,
    created_items: usize,
    episode_created: bool,
}

fn episode_hierarchy(relative_path: &str, parsed: &ParsedEpisodeFilename) -> EpisodeHierarchy {
    let components = relative_path
        .split(['/', '\\'])
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    let directories = components
        .split_last()
        .map(|(_, directories)| directories)
        .unwrap_or(&[]);
    let season_directory_index = directories
        .iter()
        .rposition(|value| parse_season_directory_name(value).is_some());
    let series_components = season_directory_index
        .map(|index| &directories[..index])
        .unwrap_or(directories);
    let series_path = if series_components.is_empty() {
        "Series".to_owned()
    } else {
        series_components.join("/")
    };
    let parsed_series = series_components
        .last()
        .and_then(|value| parse_media_name(value, MediaKind::Series));
    let series_title = parsed_series
        .as_ref()
        .map(|value| value.title.clone())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Series".to_owned());
    let production_year = parsed_series
        .as_ref()
        .and_then(|value| value.production_year)
        .or(parsed.production_year);
    let provider_ids = parsed_series
        .map(|value| value.provider_ids.clone())
        .filter(|provider_ids| !provider_ids.is_empty())
        .unwrap_or_else(|| parsed.provider_ids.clone());
    let season_number = season_directory_number(directories).unwrap_or(parsed.season);
    EpisodeHierarchy {
        series_path,
        series_title,
        production_year,
        provider_ids,
        season_number,
    }
}

fn legacy_series_identity(
    root: &StoredLibraryRoot,
    hierarchy: &EpisodeHierarchy,
) -> Option<String> {
    hierarchy
        .series_path
        .split('/')
        .next()
        .filter(|component| !component.is_empty())
        .map(|component| format!("series:{}:{component}", root.id))
}

fn season_directory_number(components: &[&str]) -> Option<u32> {
    components
        .iter()
        .rev()
        .find_map(|value| parse_season_directory_name(value))
}

fn parse_season_directory_name(value: &str) -> Option<u32> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized == "specials" {
        return Some(0);
    }
    let digits = normalized
        .strip_prefix("season")
        .or_else(|| normalized.strip_prefix('s'))?
        .trim();
    let digits = if let Some((prefix, suffix)) = digits.split_once('(') {
        suffix.strip_suffix(')').filter(|value| {
            !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
        })?;
        prefix.trim()
    } else {
        digits
    };
    digits.parse::<u32>().ok()
}

struct FileBatchWalker {
    directories: Vec<PathBuf>,
    current: Option<(PathBuf, fs::ReadDir)>,
}

impl FileBatchWalker {
    fn new(root: &Path) -> Self {
        Self {
            directories: vec![root.to_owned()],
            current: None,
        }
    }

    async fn next_batch(
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
                    if file_type.is_file() && is_supported_movie_file(&path) {
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

#[derive(Debug)]
struct ReconciliationDirectoryEntries {
    directories: Vec<String>,
    media_files: Vec<String>,
}

async fn discover_reconciliation_directory(
    root: &StoredLibraryRoot,
    relative_directory: &str,
    cancellation: &AtomicBool,
) -> Result<Option<ReconciliationDirectoryEntries>, ScannerError> {
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
    let mut entries = fs::read_dir(&directory_path)
        .await
        .map_err(|source| ScannerError::Io {
            path: directory_path.clone(),
            source,
        })?;
    let mut discovered = ReconciliationDirectoryEntries {
        directories: Vec::new(),
        media_files: Vec::new(),
    };
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
        if !(file_type.is_dir() || file_type.is_file() && is_supported_movie_file(&path)) {
            continue;
        }
        let relative_path = path
            .strip_prefix(root_path)
            .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
            .to_str()
            .ok_or(ScannerError::NonUtf8Path)?
            .to_owned();
        if file_type.is_dir() {
            discovered.directories.push(relative_path);
        } else {
            discovered.media_files.push(relative_path);
        }
    }
    discovered.directories.sort();
    discovered.media_files.sort();
    Ok(Some(discovered))
}

fn is_supported_movie_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "mkv" | "mp4" | "strm"
            )
        })
}

fn is_strm_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"))
}

async fn read_strm_target(path: &Path) -> Result<StrmTarget, ScannerError> {
    let contents = fs::read_to_string(path)
        .await
        .map_err(|source| ScannerError::Io {
            path: path.to_owned(),
            source,
        })?;
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

fn safe_scan_activity_label(relative_path: &str) -> Option<String> {
    let trimmed = relative_path.trim_matches(|character| character == '/' || character == '\\');
    if trimmed.is_empty() {
        return None;
    }
    trimmed
        .rsplit(['/', '\\'])
        .find(|part| !part.trim().is_empty())
        .map(|part| {
            let mut label = part.trim().chars().take(160).collect::<String>();
            if label.contains('?') {
                label.truncate(label.find('?').unwrap_or(label.len()));
            }
            label
        })
        .filter(|label| !label.is_empty())
}

#[derive(Debug)]
pub enum ScannerError {
    LibraryNotFound,
    InvalidRootId(String),
    InvalidItemId(String),
    InvalidRelativePath(String),
    NonUtf8Path,
    FileSizeOverflow(PathBuf),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Storage(StorageError),
}

impl ScannerError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::LibraryNotFound => "LIBRARY_NOT_FOUND",
            Self::InvalidRootId(_) => "INVALID_ROOT_ID",
            Self::InvalidItemId(_) => "INVALID_ITEM_ID",
            Self::InvalidRelativePath(_) => "INVALID_RELATIVE_PATH",
            Self::NonUtf8Path => "NON_UTF8_PATH",
            Self::FileSizeOverflow(_) => "FILE_SIZE_OVERFLOW",
            Self::Io { .. } => "SCAN_IO",
            Self::Storage(_) => "STORAGE_ERROR",
        }
    }
}

impl fmt::Display for ScannerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::InvalidRootId(error) => write!(formatter, "invalid library root ID: {error}"),
            Self::InvalidItemId(error) => write!(formatter, "invalid media item ID: {error}"),
            Self::InvalidRelativePath(error) => write!(formatter, "invalid relative path: {error}"),
            Self::NonUtf8Path => formatter.write_str("path is not valid UTF-8"),
            Self::FileSizeOverflow(path) => {
                write!(formatter, "file size overflows i64: {}", path.display())
            }
            Self::Io { path, source } => {
                write!(formatter, "scan path '{}': {source}", path.display())
            }
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ScannerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Storage(error) => Some(error),
            Self::LibraryNotFound
            | Self::InvalidRootId(_)
            | Self::InvalidItemId(_)
            | Self::InvalidRelativePath(_)
            | Self::NonUtf8Path
            | Self::FileSizeOverflow(_) => None,
        }
    }
}

impl From<StorageError> for ScannerError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{media_source_folder, safe_scan_activity_label};

    #[test]
    fn media_source_folder_uses_the_source_parent_directory() {
        assert_eq!(
            media_source_folder("Movies/Dune/Dune.2021.mkv").unwrap(),
            "Movies/Dune"
        );
        assert_eq!(media_source_folder("Dune.2021.mkv").unwrap(), ".");
    }

    #[test]
    fn scan_activity_label_uses_only_the_relative_basename() {
        assert_eq!(
            safe_scan_activity_label("Movies/Dune/Dune.2021.mkv").as_deref(),
            Some("Dune.2021.mkv")
        );
        assert_eq!(
            safe_scan_activity_label("/private/root/Secret.strm?token=abc").as_deref(),
            Some("Secret.strm")
        );
        assert_eq!(safe_scan_activity_label("/"), None);
    }
}
