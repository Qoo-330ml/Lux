use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use quick_xml::{events::Event, reader::Reader};
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
        admin_events::{AdminEventHub, AdminEventScope},
        library_covers::{AutoLibraryCoverResult, LibraryCoverService},
        media_matching::{MediaKind, clean_title, has_multi_part_marker, parse_media_name},
        metadata::MetadataEnricher,
        probe::MediaProbeService,
        reidentify::{MetadataRefreshMode, MetadataReidentifyError, MetadataReidentifyService},
        thumbnails::ThumbnailService,
        watch::ChangeKind,
    },
    domain::ids::{FilesystemEntryId, ItemId, LibraryId, SourceId},
    observability::resources::ResourceMetrics,
    storage::{
        Database, NewFilesystemEntry, NewHierarchyItem, NewMediaItem, NewMediaSource, NewMovieFile,
        NewScanJobEvent, StorageError, StoredFilesystemEntry, StoredLibraryRoot,
        StoredReconciliationScanEntry, StoredScanJob, StoredScanJobPath,
    },
};

#[derive(Clone)]
pub struct LibraryScanner {
    database: Database,
}

impl LibraryScanner {
    pub fn new(database: Database) -> Self {
        Self { database }
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
            let files = match collect_movie_files(&root_path).await {
                Ok(files) => files,
                Err(ScannerError::Io { .. }) => {
                    self.database
                        .update_library_root_availability(&root.id, false)
                        .await?;
                    report.unavailable_roots += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let existing_entries = self
                .database
                .list_filesystem_entries_for_root(&root.id)
                .await?
                .into_iter()
                .map(|entry| (entry.relative_path.clone(), entry))
                .collect::<HashMap<_, _>>();
            let mut seen_entry_ids = Vec::new();
            let mut pending_new_files = Vec::with_capacity(500);
            for path in files {
                if let Some((entry_id, quick_report)) = self
                    .scan_movie_file_if_unchanged(
                        &library_id_text,
                        &root_path,
                        &path,
                        &existing_entries,
                    )
                    .await?
                {
                    seen_entry_ids.push(entry_id);
                    report.merge(quick_report);
                } else if !existing_entries.contains_key(
                    path.strip_prefix(&root_path)
                        .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                        .to_str()
                        .ok_or(ScannerError::NonUtf8Path)?,
                ) {
                    if let Some(file) = self.prepare_new_movie_file(&root_path, &path).await? {
                        pending_new_files.push(file);
                        if pending_new_files.len() == 500 {
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
            let files = collect_series_files(&root_path).await?;
            for path in files {
                report.merge(
                    self.scan_episode_file(&library_id_text, &root, &root_path, &path, &generation)
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
            for path in collect_series_files(&root_path).await? {
                let classification = classify_mixed_file(&root_path, &path).await;
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
                        self.scan_episode_file(
                            &library_id_text,
                            &root,
                            &root_path,
                            &path,
                            &generation,
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
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(ScanReport::default());
        };
        let Some(parsed) = parse_episode_filename(file_name) else {
            return Ok(ScanReport::default());
        };
        let is_strm = is_strm_file(path);
        let external_url = if is_strm {
            read_strm_url(path).await?
        } else {
            None
        };
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
        let fingerprint_unchanged = existing_entry
            .as_ref()
            .is_some_and(|entry| entry.fingerprint.as_deref() == Some(fingerprint.as_slice()));
        let episode_is_current = if fingerprint_unchanged {
            let hierarchy = episode_hierarchy(&relative_path, &parsed);
            let identity = Self::episode_identity_key(root, &hierarchy, &parsed);
            self.database
                .find_media_item_by_identity(&identity)
                .await?
                .is_some_and(|item| {
                    existing_entry
                        .as_ref()
                        .and_then(|entry| entry.item_id.as_deref())
                        == Some(item.id.as_str())
                })
        } else {
            false
        };
        if episode_is_current && let Some(existing_entry) = existing_entry.as_ref() {
            if is_strm {
                self.database
                    .update_media_source_external_url(&existing_entry.id, external_url.as_deref())
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

        let hierarchy = episode_hierarchy(&relative_path, &parsed);
        let ensured = self
            .ensure_episode_hierarchy(library_id_text, root, &parsed, &hierarchy)
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
                        .update_media_source_external_url(
                            &existing_entry.id,
                            external_url.as_deref(),
                        )
                        .await?;
                }
                self.database
                    .mark_filesystem_entry_seen(&existing_entry.id, generation)
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
                .reset_media_probe_for_filesystem_entry(&existing_entry.id, size)
                .await?;
            if is_strm {
                self.database
                    .update_media_source_external_url(&existing_entry.id, external_url.as_deref())
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
                external_url: external_url.as_deref(),
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
        let external_url = if is_strm {
            read_strm_url(path).await?
        } else {
            None
        };
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
                if is_strm {
                    self.database
                        .update_media_source_external_url(
                            &existing_entry.id,
                            external_url.as_deref(),
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
                    .update_media_source_external_url(&existing_entry.id, external_url.as_deref())
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
                external_url: external_url.as_deref(),
                is_default: true,
            })
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
        parsed: &ParsedEpisodeFilename,
        hierarchy: &EpisodeHierarchy,
    ) -> Result<EnsuredEpisodeHierarchy, ScannerError> {
        let series_sort_title = hierarchy.series_title.to_lowercase();
        let series_identity = format!("series:{}:{}", root.id, hierarchy.series_path);
        let series_new_id = ItemId::new().to_string();
        let (series_id, series_created) = self
            .ensure_hierarchy_item(NewHierarchyItem {
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
                identification_status: "PENDING",
                identity_key: &series_identity,
            })
            .await?;
        let season_title = if hierarchy.season_number == 0 {
            "Specials".to_owned()
        } else {
            format!("Season {:02}", hierarchy.season_number)
        };
        let season_identity = format!("{series_identity}:season:{}", hierarchy.season_number);
        let season_sort_title = season_title.to_lowercase();
        let season_new_id = ItemId::new().to_string();
        let (season_id, season_created) = self
            .ensure_hierarchy_item(NewHierarchyItem {
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
                identification_status: "PENDING",
                identity_key: &season_identity,
            })
            .await?;
        let episode_identity = Self::episode_identity_key(root, hierarchy, parsed);
        let episode_title = parsed.title.clone();
        let episode_sort_title = episode_title.to_lowercase();
        let episode_new_id = ItemId::new().to_string();
        let (episode_id, episode_created) = self
            .ensure_hierarchy_item(NewHierarchyItem {
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
                identification_status: "PENDING",
                identity_key: &episode_identity,
            })
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
        let files = collect_movie_files(&canonical_directory).await?;
        let mut report = ScanReport::default();
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
        Ok(report)
    }

    async fn scan_movie_file_if_unchanged(
        &self,
        library_id_text: &str,
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

    async fn prepare_new_movie_file(
        &self,
        root_path: &Path,
        path: &Path,
    ) -> Result<Option<NewMovieFile>, ScannerError> {
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let Some(parsed_name) = parse_movie_filename(file_name) else {
            return Ok(None);
        };
        let is_strm = is_strm_file(path);
        let external_url = if is_strm {
            read_strm_url(path).await?
        } else {
            None
        };
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
            source_kind: if is_strm {
                "STRM_URL".to_owned()
            } else {
                "LOCAL_FILE".to_owned()
            },
            edition_name: parsed_name.edition_name,
            quality_label: parsed_name.quality_label,
            container,
            external_url,
        }))
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
        let is_strm = is_strm_file(path);
        let external_url = if is_strm {
            read_strm_url(path).await?
        } else {
            None
        };
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
        if !has_multi_part_marker(file_name)
            && let Some(existing_entry) = existing_entry.as_ref()
        {
            if existing_entry.fingerprint.as_deref() == Some(fingerprint.as_slice()) {
                if is_strm {
                    self.database
                        .update_media_source_external_url(
                            &existing_entry.id,
                            external_url.as_deref(),
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
                    .update_media_source_external_url(&existing_entry.id, external_url.as_deref())
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
                })
                .await?;
            (item_id, true)
        };
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
                        .update_media_source_external_url(
                            &existing_entry.id,
                            external_url.as_deref(),
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
                    .update_media_source_external_url(&existing_entry.id, external_url.as_deref())
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
                external_url: external_url.as_deref(),
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
    scan_lock: Arc<Semaphore>,
    library_covers: Option<LibraryCoverService>,
    resources: ResourceMetrics,
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
            scan_lock: Arc::new(Semaphore::new(1)),
            library_covers: None,
            resources: ResourceMetrics::new(),
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

    pub fn with_library_covers(mut self, library_covers: LibraryCoverService) -> Self {
        self.library_covers = Some(library_covers);
        self
    }

    pub fn with_resource_metrics(mut self, resources: ResourceMetrics) -> Self {
        self.resources = resources;
        self
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
        let job = if let Some(active) = self
            .database
            .find_active_scan_job(&library_id_text, "INCREMENTAL_SCAN")
            .await?
        {
            active
        } else {
            let id = Uuid::now_v7().to_string();
            let generation = Uuid::now_v7().to_string();
            self.database
                .create_scan_job(&id, &library_id_text, "INCREMENTAL_SCAN", &generation, 0)
                .await?;
            self.database
                .find_scan_job(&id)
                .await?
                .ok_or(ScanJobError::JobNotFound)?
        };
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

    pub async fn create_movie_scan_job(
        &self,
        library_id: LibraryId,
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
            .find_active_scan_job(&library_id_text, "RECONCILE_LIBRARY")
            .await?
        {
            return Err(ScanJobError::AlreadyActive(active.id));
        }
        let roots = self.database.list_library_roots(&library_id_text).await?;
        let id = Uuid::now_v7().to_string();
        let generation = Uuid::now_v7().to_string();
        let root_ids = roots.into_iter().map(|root| root.id).collect::<Vec<_>>();
        self.database
            .create_reconciliation_scan_job(&id, &library_id_text, &generation, &root_ids)
            .await?;
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
        self.run_batch_unlocked(job_id, batch_size).await
    }

    async fn run_batch_unlocked(
        &self,
        job_id: &str,
        batch_size: usize,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Err(ScanJobError::JobNotFound);
        };
        if job.job_type == "INCREMENTAL_SCAN" {
            return self.run_incremental_batch(job_id, batch_size).await;
        }
        if matches!(job.status.as_str(), "COMPLETED" | "CANCELLED" | "FAILED") {
            return Ok(ScanBatchReport {
                status: job.status,
                processed: 0,
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
        if self.database.scan_job_cancel_requested(job_id).await? {
            self.database
                .clear_reconciliation_scan_entries(job_id)
                .await?;
            self.database
                .finish_scan_job(job_id, "CANCELLED", None)
                .await?;
            self.record_event(job_id, "INFO", "JOB_CANCELLED", "任务已取消", "{}")
                .await;
            return Ok(ScanBatchReport {
                status: "CANCELLED".to_owned(),
                processed: 0,
                completed: true,
            });
        }

        if !job.discovery_completed {
            return self
                .run_reconciliation_discovery_batch(&job, batch_size)
                .await;
        }
        self.run_reconciliation_file_batch(&job, batch_size).await
    }

    async fn run_reconciliation_discovery_batch(
        &self,
        job: &StoredScanJob,
        batch_size: usize,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let limit = i64::try_from(batch_size).unwrap_or(i64::MAX);
        let directories = self
            .database
            .list_reconciliation_scan_entries(&job.id, "DIRECTORY", limit)
            .await?;
        let mut unavailable_root_ids = HashSet::new();
        for directory in directories {
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
            match discover_reconciliation_directory(&root, &directory.relative_path).await {
                Ok(discovered) => {
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
                Err(error) => return self.fail_reconciliation_job(job, error).await,
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
            let details = format!(r#"{{"total":{total}}}"#);
            self.record_event(
                &job.id,
                "INFO",
                "DISCOVERY_COMPLETED",
                "媒体库目录发现完成",
                &details,
            )
            .await;
        }
        Ok(ScanBatchReport {
            status: "RUNNING".to_owned(),
            processed: 0,
            completed: false,
        })
    }

    async fn run_reconciliation_file_batch(
        &self,
        job: &StoredScanJob,
        batch_size: usize,
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
        if batch.is_empty() {
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
                self.database
                    .mark_missing_filesystem_entries(&root.id, &job.generation)
                    .await?;
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
            self.database
                .finish_scan_job(&job.id, "COMPLETED", None)
                .await?;
            self.record_event(&job.id, "INFO", "JOB_COMPLETED", "任务已完成", "{}")
                .await;
            return Ok(ScanBatchReport {
                status: "COMPLETED".to_owned(),
                processed: 0,
                completed: true,
            });
        }

        if library_kind == "MOVIE" {
            return self
                .run_movie_reconciliation_file_batch(job, &roots, &batch, scan_concurrency)
                .await;
        }

        let mut processed = 0_usize;
        let mut next_count = job.processed_count;
        let mut completed_entries = Vec::<StoredReconciliationScanEntry>::new();
        for entry in &batch {
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
            if let Err(error) = result {
                return self.fail_reconciliation_job(job, error).await;
            }
            next_count = next_count.saturating_add(1);
            processed = processed.saturating_add(1);
            completed_entries.push(entry.clone());
        }
        self.finish_reconciliation_file_batch(job, completed_entries, processed, next_count, None)
            .await
    }

    async fn run_movie_reconciliation_file_batch(
        &self,
        job: &StoredScanJob,
        roots: &[StoredLibraryRoot],
        batch: &[StoredReconciliationScanEntry],
        configured_concurrency: i64,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let mut processed = 0_usize;
        let mut next_count = job.processed_count;
        let mut completed_entries = Vec::<(usize, StoredReconciliationScanEntry)>::new();
        let mut unavailable_root_ids = HashSet::<String>::new();
        let mut existing_paths_by_root = HashMap::<String, HashSet<String>>::new();
        let mut batch_paths_by_root = HashMap::<String, Vec<String>>::new();
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
            let existing_paths = self
                .database
                .list_existing_filesystem_paths(&root_id, &paths)
                .await?
                .into_iter()
                .collect();
            existing_paths_by_root.insert(root_id, existing_paths);
        }

        for (index, entry) in batch.iter().enumerate() {
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

            if existing_paths_by_root
                .get(&root.id)
                .is_some_and(|paths| paths.contains(&entry.relative_path))
            {
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
                    return self.fail_reconciliation_job(job, error).await;
                }
                next_count = next_count.saturating_add(1);
                processed = processed.saturating_add(1);
                completed_entries.push((index, entry.clone()));
                continue;
            }

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
            if active_tasks >= concurrency {
                let prepared = join_movie_preparation(&mut preparation_tasks).await;
                let (index, root_id, entry, file) = match prepared {
                    Ok(result) => result,
                    Err(error) => return self.fail_reconciliation_job(job, error).await,
                };
                active_tasks = active_tasks.saturating_sub(1);
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
                Err(error) => return self.fail_reconciliation_job(job, error).await,
            };
            active_tasks = active_tasks.saturating_sub(1);
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
            if let Err(error) = self
                .database
                .insert_movie_files_batch(&job.library_id, &root.id, &job.generation, files)
                .await
            {
                return self.fail_reconciliation_job(job, error.into()).await;
            }
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
            self.database
                .clear_reconciliation_scan_entries(&job.id)
                .await?;
            self.database
                .finish_scan_job(&job.id, "CANCELLED", None)
                .await?;
            self.record_event(&job.id, "INFO", "JOB_CANCELLED", "任务已取消", "{}")
                .await;
            return Ok(ScanBatchReport {
                status: "CANCELLED".to_owned(),
                processed,
                completed: true,
            });
        }
        Ok(ScanBatchReport {
            status: "RUNNING".to_owned(),
            processed,
            completed: false,
        })
    }

    async fn fail_reconciliation_job(
        &self,
        job: &StoredScanJob,
        error: ScannerError,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let error_code = error.code();
        self.database
            .clear_reconciliation_scan_entries(&job.id)
            .await?;
        self.database
            .finish_scan_job(&job.id, "FAILED", Some(&error.to_string()))
            .await?;
        self.record_event(&job.id, "ERROR", error_code, "扫描任务失败", "{}")
            .await;
        Err(error.into())
    }

    async fn run_incremental_batch(
        &self,
        job_id: &str,
        batch_size: usize,
    ) -> Result<ScanBatchReport, ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Err(ScanJobError::JobNotFound);
        };
        if matches!(job.status.as_str(), "COMPLETED" | "CANCELLED" | "FAILED") {
            return Ok(ScanBatchReport {
                status: job.status,
                processed: 0,
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
        if self.database.scan_job_cancel_requested(job_id).await? {
            self.database
                .finish_scan_job(job_id, "CANCELLED", None)
                .await?;
            self.record_event(job_id, "INFO", "JOB_CANCELLED", "扫描任务已取消", "{}")
                .await;
            return Ok(ScanBatchReport {
                status: "CANCELLED".to_owned(),
                processed: 0,
                completed: true,
            });
        }
        let paths = self
            .database
            .list_pending_scan_job_paths(job_id, i64::try_from(batch_size).unwrap_or(i64::MAX))
            .await?;
        if paths.is_empty() {
            if self.database.finish_scan_job_if_idle(job_id).await? {
                self.database
                    .update_library_last_scan(&job.library_id)
                    .await?;
                self.record_event(job_id, "INFO", "JOB_COMPLETED", "局部扫描任务已完成", "{}")
                    .await;
                return Ok(ScanBatchReport {
                    status: "COMPLETED".to_owned(),
                    processed: 0,
                    completed: true,
                });
            }
            return Ok(ScanBatchReport {
                status: "RUNNING".to_owned(),
                processed: 0,
                completed: false,
            });
        }
        let library = self
            .database
            .find_library(&job.library_id)
            .await?
            .ok_or(ScanJobError::LibraryNotFound)?;
        for path in &paths {
            if let Err(error) = self
                .process_incremental_path(&library.kind, &job, path)
                .await
            {
                self.database
                    .finish_scan_job(job_id, "FAILED", Some(&error.to_string()))
                    .await?;
                self.record_event(job_id, "ERROR", error.code(), "局部扫描任务失败", "{}")
                    .await;
                return Err(error.into());
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
            completed: false,
        })
    }

    async fn process_incremental_path(
        &self,
        library_kind: &str,
        job: &StoredScanJob,
        path: &StoredScanJobPath,
    ) -> Result<(), ScannerError> {
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
            return Ok(());
        }
        let metadata = fs::metadata(&media_path)
            .await
            .map_err(|source| ScannerError::Io {
                path: media_path.clone(),
                source,
            })?;
        let generation = &job.generation;
        let files = if metadata.is_dir() {
            if library_kind == "SERIES" {
                collect_series_files(&media_path).await?
            } else {
                collect_movie_files(&media_path).await?
            }
        } else if is_supported_movie_file(&media_path) {
            vec![media_path]
        } else {
            Vec::new()
        };
        for file in files {
            match library_kind {
                "MOVIE" => {
                    self.scanner
                        .scan_movie_file(&job.library_id, &root, root_path, &file, generation)
                        .await?;
                }
                "SERIES" => {
                    self.scanner
                        .scan_episode_file(&job.library_id, &root, root_path, &file, generation)
                        .await?;
                }
                "MIXED" => match classify_mixed_file(root_path, &file).await {
                    MixedClassification::Movie => {
                        self.scanner
                            .scan_movie_file(&job.library_id, &root, root_path, &file, generation)
                            .await?;
                    }
                    MixedClassification::Episode => {
                        self.scanner
                            .scan_episode_file(&job.library_id, &root, root_path, &file, generation)
                            .await?;
                    }
                    MixedClassification::Unresolved => {
                        self.scanner
                            .scan_unresolved_file(
                                &job.library_id,
                                &root,
                                root_path,
                                &file,
                                generation,
                            )
                            .await?;
                    }
                },
                _ => return Err(ScannerError::LibraryNotFound),
            }
        }
        Ok(())
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
        loop {
            let report = self.run_batch_unlocked(job_id, batch_size).await?;
            if !report.completed {
                tokio::task::yield_now().await;
                continue;
            }
            if report.status == "COMPLETED" {
                let incremental = self
                    .database
                    .find_scan_job(job_id)
                    .await?
                    .is_some_and(|job| job.job_type == "INCREMENTAL_SCAN");
                if incremental {
                    self.run_auto_library_cover_after_scan(job_id).await?;
                    return Ok(());
                }
                self.run_probe_after_scan(job_id, probe).await?;
                self.run_metadata_after_scan(job_id).await?;
                self.run_thumbnails_after_scan(job_id, thumbnails).await?;
                self.run_auto_library_cover_after_scan(job_id).await?;
                if let Some(metadata) = metadata {
                    self.schedule_online_metadata_after_scan(job_id, metadata)
                        .await;
                }
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
        let available_parallelism = std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1);
        let container_cpu_limit = self.resources.cpu_limit_cores().await;
        recommended_scan_concurrency(
            configured,
            available_parallelism,
            self.resources.home_latency_p95_ms(),
            container_cpu_limit,
        )
    }

    async fn run_thumbnails_after_scan(
        &self,
        job_id: &str,
        thumbnails: Option<ThumbnailService>,
    ) -> Result<(), ScanJobError> {
        let Some(thumbnails) = thumbnails else {
            return Ok(());
        };
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Err(ScanJobError::JobNotFound);
        };
        let Ok(library_id) = job.library_id.parse::<LibraryId>() else {
            tracing::warn!(
                job_id,
                library_id = %job.library_id,
                "scan thumbnails skipped for invalid library ID"
            );
            return Ok(());
        };
        match thumbnails.generate_library(library_id).await {
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

    async fn run_metadata_after_scan(&self, job_id: &str) -> Result<(), ScanJobError> {
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Err(ScanJobError::JobNotFound);
        };
        let Some(library) = self.database.find_library(&job.library_id).await? else {
            return Err(ScanJobError::LibraryNotFound);
        };
        let Ok(library_id) = job.library_id.parse::<LibraryId>() else {
            tracing::warn!(job_id, library_id = %job.library_id, "local metadata enrichment skipped for invalid library ID");
            return Ok(());
        };
        let enricher = MetadataEnricher::new(self.database.clone());
        let result = match library.kind.as_str() {
            "MOVIE" => enricher.enrich_movie_library(library_id).await,
            "SERIES" => enricher.enrich_series_library(library_id).await,
            "MIXED" => enricher.enrich_mixed_library(library_id).await,
            _ => return Ok(()),
        };
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

    async fn run_probe_after_scan(
        &self,
        job_id: &str,
        probe: Option<MediaProbeService>,
    ) -> Result<(), ScanJobError> {
        let Some(probe) = probe else {
            return Ok(());
        };
        let Some(job) = self.database.find_scan_job(job_id).await? else {
            return Err(ScanJobError::JobNotFound);
        };
        let Ok(library_id) = job.library_id.parse::<LibraryId>() else {
            tracing::warn!(job_id, library_id = %job.library_id, "scan probe skipped for invalid library ID");
            return Ok(());
        };
        match probe.probe_movie_library(library_id).await {
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
        self.database.request_scan_job_cancel(job_id).await?;
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
        self.create_movie_scan_job(library_id).await
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

const HOME_P95_DEGRADED_MS: u64 = 300;
const HOME_P95_TARGET_MS: u64 = 400;

fn recommended_scan_concurrency(
    configured: i64,
    available_parallelism: usize,
    home_p95_ms: Option<u64>,
    container_cpu_limit: Option<f64>,
) -> usize {
    let configured = usize::try_from(configured).unwrap_or(1).max(1);
    let container_parallelism = container_cpu_limit
        .filter(|limit| limit.is_finite() && *limit > 0.0)
        .map_or(available_parallelism, |limit| {
            limit.ceil().min(usize::MAX as f64) as usize
        });
    let cpu_cap = container_parallelism.saturating_sub(1).max(1);
    let base = configured.min(cpu_cap);
    match home_p95_ms {
        Some(value) if value >= HOME_P95_TARGET_MS => 1,
        Some(value) if value >= HOME_P95_DEGRADED_MS => base.div_ceil(2).max(1),
        _ => base,
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

#[cfg(test)]
mod resource_tests {
    use super::recommended_scan_concurrency;

    #[test]
    fn reserves_one_parallel_unit_for_foreground_requests() {
        assert_eq!(recommended_scan_concurrency(8, 8, None, None), 7);
        assert_eq!(recommended_scan_concurrency(8, 1, None, None), 1);
    }

    #[test]
    fn slows_scan_when_home_latency_degrades() {
        assert_eq!(recommended_scan_concurrency(8, 8, Some(300), None), 4);
        assert_eq!(recommended_scan_concurrency(8, 8, Some(400), None), 1);
        assert_eq!(recommended_scan_concurrency(1, 8, Some(300), None), 1);
    }

    #[test]
    fn respects_the_container_cpu_limit() {
        assert_eq!(recommended_scan_concurrency(8, 16, None, Some(4.0)), 3);
        assert_eq!(recommended_scan_concurrency(8, 16, None, Some(0.5)), 1);
    }
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
    pub cancel_requested: bool,
    pub error: Option<String>,
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
        cancel_requested: job.cancel_requested,
        error: job.error,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanBatchReport {
    pub status: String,
    pub processed: usize,
    pub completed: bool,
}

#[derive(Debug)]
pub enum ScanJobError {
    LibraryNotFound,
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
}

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
    })
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
        .and_then(|value| value.production_year)
        .or(parsed.production_year);
    let season_number = season_directory_number(directories).unwrap_or(parsed.season);
    EpisodeHierarchy {
        series_path,
        series_title,
        production_year,
        season_number,
    }
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

pub(crate) async fn collect_movie_files(root: &Path) -> Result<Vec<PathBuf>, ScannerError> {
    let mut directories = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(directory) = directories.pop() {
        let mut entries = fs::read_dir(&directory)
            .await
            .map_err(|source| ScannerError::Io {
                path: directory.clone(),
                source,
            })?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| ScannerError::Io {
                path: directory.clone(),
                source,
            })?
        {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|source| ScannerError::Io {
                path: path.clone(),
                source,
            })?;
            if file_type.is_file() && is_supported_movie_file(&path) {
                files.push(path);
            } else if file_type.is_dir() {
                directories.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

#[derive(Debug)]
struct ReconciliationDirectoryEntries {
    directories: Vec<String>,
    media_files: Vec<String>,
}

async fn discover_reconciliation_directory(
    root: &StoredLibraryRoot,
    relative_directory: &str,
) -> Result<ReconciliationDirectoryEntries, ScannerError> {
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
        let path = entry.path();
        let file_type = entry.file_type().await.map_err(|source| ScannerError::Io {
            path: path.clone(),
            source,
        })?;
        if !file_type.is_dir() && !(file_type.is_file() && is_supported_movie_file(&path)) {
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
    Ok(discovered)
}

async fn collect_series_files(root: &Path) -> Result<Vec<PathBuf>, ScannerError> {
    let mut directories = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(directory) = directories.pop() {
        let mut entries = fs::read_dir(&directory)
            .await
            .map_err(|source| ScannerError::Io {
                path: directory.clone(),
                source,
            })?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| ScannerError::Io {
                path: directory.clone(),
                source,
            })?
        {
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|source| ScannerError::Io {
                path: path.clone(),
                source,
            })?;
            if file_type.is_file() && is_supported_movie_file(&path) {
                files.push(path);
            } else if file_type.is_dir() {
                directories.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
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

async fn read_strm_url(path: &Path) -> Result<Option<String>, ScannerError> {
    let contents = fs::read_to_string(path)
        .await
        .map_err(|source| ScannerError::Io {
            path: path.to_owned(),
            source,
        })?;
    Ok(contents.lines().find_map(|line| {
        let line = line.trim().trim_start_matches('\u{feff}').trim();
        (!line.is_empty()).then(|| line.to_owned())
    }))
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
