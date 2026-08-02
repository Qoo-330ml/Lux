use std::{
    fmt,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use quick_xml::{events::Event, reader::Reader};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use tokio::fs;
use uuid::Uuid;

use crate::{
    domain::ids::{FilesystemEntryId, ItemId, LibraryId, SourceId},
    storage::{
        Database, NewFilesystemEntry, NewHierarchyItem, NewMediaItem, NewMediaSource,
        NewScanJobEvent, StorageError, StoredLibraryRoot, StoredScanJob,
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
            for path in files {
                report.merge(
                    self.scan_movie_file(&library_id_text, &root, &root_path, &path, &generation)
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

        let components = relative_path
            .split(['/', '\\'])
            .filter(|component| !component.is_empty())
            .collect::<Vec<_>>();
        let series_name = components.first().copied().unwrap_or("Series");
        let series_title = clean_hierarchy_title(series_name);
        let series_sort_title = series_title.to_lowercase();
        let series_identity = format!("series:{}:{}", root.id, series_name);
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
                title: &series_title,
                sort_title: &series_sort_title,
                original_title: Some(&series_title),
                production_year: None,
                identification_status: "LOCAL_CONFIRMED",
                identity_key: &series_identity,
            })
            .await?;
        let season_number = season_directory_number(&components).unwrap_or(parsed.season);
        let season_title = if season_number == 0 {
            "Specials".to_owned()
        } else {
            format!("Season {season_number:02}")
        };
        let season_identity = format!("{series_identity}:season:{season_number}");
        let season_sort_title = season_title.to_lowercase();
        let season_new_id = ItemId::new().to_string();
        let (season_id, season_created) = self
            .ensure_hierarchy_item(NewHierarchyItem {
                id: &season_new_id,
                library_id: library_id_text,
                item_type: "SEASON",
                parent_id: Some(&series_id),
                series_id: Some(&series_id),
                season_number: Some(i64::from(season_number)),
                episode_number: None,
                absolute_number: None,
                title: &season_title,
                sort_title: &season_sort_title,
                original_title: Some(&season_title),
                production_year: None,
                identification_status: "LOCAL_CONFIRMED",
                identity_key: &season_identity,
            })
            .await?;
        let episode_identity = format!("episode:{}:{}", root.id, relative_path);
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
                season_number: Some(i64::from(season_number)),
                episode_number: Some(i64::from(parsed.episode)),
                absolute_number: parsed.absolute_number.map(i64::from),
                title: &episode_title,
                sort_title: &episode_sort_title,
                original_title: Some(&episode_title),
                production_year: None,
                identification_status: "LOCAL_CONFIRMED",
                identity_key: &episode_identity,
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
                item_id: &episode_id,
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
            created_items: usize::from(series_created)
                + usize::from(season_created)
                + usize::from(episode_created),
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
            return Ok((existing.id, false));
        }
        let id = item.id.to_owned();
        self.database.insert_hierarchy_item(item).await?;
        Ok((id, true))
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
        let existing_item = self
            .database
            .find_media_item(
                library_id_text,
                &parsed_name.sort_title,
                parsed_name.production_year.map(i64::from),
            )
            .await?;
        let (item_id, created_item) = if let Some(item) = existing_item {
            (
                item.id
                    .parse::<ItemId>()
                    .map_err(|error| ScannerError::InvalidItemId(error.to_string()))?,
                false,
            )
        } else {
            let item_id = ItemId::new();
            let item_id_text = item_id.to_string();
            self.database
                .insert_media_item(NewMediaItem {
                    id: &item_id_text,
                    library_id: library_id_text,
                    title: &parsed_name.title,
                    sort_title: &parsed_name.sort_title,
                    original_title: Some(&parsed_name.title),
                    production_year: parsed_name.production_year.map(i64::from),
                })
                .await?;
            (item_id, true)
        };
        let item_id_text = item_id.to_string();
        let source_id = SourceId::new().to_string();
        let container = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        self.database
            .insert_media_source(NewMediaSource {
                id: &source_id,
                item_id: &item_id_text,
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
}

impl ScanJobService {
    pub fn new(database: Database) -> Self {
        Self {
            scanner: LibraryScanner::new(database.clone()),
            database,
        }
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
        let library_kind = library.kind;
        let mut total_count = 0_i64;
        for root in roots {
            if root.is_available {
                let files = if library_kind == "MOVIE" {
                    collect_movie_files(Path::new(&root.canonical_path)).await?
                } else {
                    collect_series_files(Path::new(&root.canonical_path)).await?
                };
                total_count =
                    total_count.saturating_add(i64::try_from(files.len()).unwrap_or(i64::MAX));
            }
        }
        let id = Uuid::now_v7().to_string();
        let generation = Uuid::now_v7().to_string();
        self.database
            .create_scan_job(
                &id,
                &library_id_text,
                "RECONCILE_LIBRARY",
                &generation,
                total_count,
            )
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
            self.record_event(job_id, "INFO", "JOB_STARTED", "任务开始执行", "{}")
                .await;
        }
        if self.database.scan_job_cancel_requested(job_id).await? {
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

        let roots = self.database.list_library_roots(&job.library_id).await?;
        let library_kind = self
            .database
            .find_library(&job.library_id)
            .await?
            .map(|library| library.kind)
            .unwrap_or_else(|| "MOVIE".to_owned());
        let mut candidates = Vec::new();
        for (root_index, root) in roots.iter().enumerate() {
            if !root.is_available {
                continue;
            }
            let root_path = Path::new(&root.canonical_path);
            let files = if library_kind == "MOVIE" {
                collect_movie_files(root_path).await?
            } else {
                collect_series_files(root_path).await?
            };
            for path in files {
                let relative = path
                    .strip_prefix(root_path)
                    .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                    .to_str()
                    .ok_or(ScannerError::NonUtf8Path)?
                    .to_owned();
                let cursor = format!("{}\0{}", root.canonical_path, relative);
                if job
                    .cursor
                    .as_deref()
                    .is_some_and(|value| cursor.as_str() <= value)
                {
                    continue;
                }
                candidates.push((cursor, root_index, path));
            }
        }
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        let batch = candidates.into_iter().take(batch_size).collect::<Vec<_>>();
        if batch.is_empty() {
            for root in &roots {
                if root.is_available {
                    self.database
                        .mark_missing_filesystem_entries(&root.id, &job.generation)
                        .await?;
                    self.database
                        .update_root_scan_cursor(&root.id, None)
                        .await?;
                }
            }
            self.database
                .update_library_last_scan(&job.library_id)
                .await?;
            self.database
                .update_scan_job_progress(job_id, None, job.processed_count)
                .await?;
            self.database
                .finish_scan_job(job_id, "COMPLETED", None)
                .await?;
            self.record_event(job_id, "INFO", "JOB_COMPLETED", "任务已完成", "{}")
                .await;
            return Ok(ScanBatchReport {
                status: "COMPLETED".to_owned(),
                processed: 0,
                completed: true,
            });
        }

        let mut processed = 0_usize;
        let mut last_cursor = None;
        for (cursor, root_index, path) in &batch {
            let root = &roots[*root_index];
            let result = match library_kind.as_str() {
                "MOVIE" => {
                    self.scanner
                        .scan_movie_file(
                            &job.library_id,
                            root,
                            Path::new(&root.canonical_path),
                            path,
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
                            path,
                            &job.generation,
                        )
                        .await
                }
                "MIXED" => match classify_mixed_file(Path::new(&root.canonical_path), path).await {
                    MixedClassification::Movie => {
                        self.scanner
                            .scan_movie_file(
                                &job.library_id,
                                root,
                                Path::new(&root.canonical_path),
                                path,
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
                                path,
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
                                path,
                                &job.generation,
                            )
                            .await
                    }
                },
                _ => Err(ScannerError::LibraryNotFound),
            };
            if let Err(error) = result {
                let error_code = error.code();
                self.database
                    .finish_scan_job(job_id, "FAILED", Some(&error.to_string()))
                    .await?;
                self.record_event(job_id, "ERROR", error_code, "扫描任务失败", "{}")
                    .await;
                return Err(error.into());
            }
            last_cursor = Some(cursor.as_str());
            processed += 1;
        }
        let next_count = job
            .processed_count
            .saturating_add(i64::try_from(processed).unwrap_or(i64::MAX));
        self.database
            .update_scan_job_progress(job_id, last_cursor, next_count)
            .await?;
        let batch_details = format!(r#"{{"processed":{processed},"total":{next_count}}}"#);
        self.record_event(
            job_id,
            "INFO",
            "BATCH_COMPLETED",
            "扫描批次完成",
            &batch_details,
        )
        .await;
        if let Some((cursor, root_index, path)) = batch.last() {
            let root = &roots[*root_index];
            let relative = path
                .strip_prefix(Path::new(&root.canonical_path))
                .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?
                .to_str()
                .ok_or(ScannerError::NonUtf8Path)?;
            self.database
                .update_root_scan_cursor(&root.id, Some(relative))
                .await?;
            let _ = cursor;
        }
        if self.database.scan_job_cancel_requested(job_id).await? {
            self.database
                .finish_scan_job(job_id, "CANCELLED", None)
                .await?;
            self.record_event(job_id, "INFO", "JOB_CANCELLED", "任务已取消", "{}")
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
    AlreadyActive(String),
    InvalidBatchSize,
    Scanner(ScannerError),
    Storage(StorageError),
}

impl std::fmt::Display for ScanJobError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::JobNotFound => formatter.write_str("scan job not found"),
            Self::AlreadyActive(id) => write!(formatter, "scan job already active: {id}"),
            Self::InvalidBatchSize => formatter.write_str("scan batch size must be positive"),
            Self::Scanner(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
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
    pub season: u32,
    pub episode: u32,
    pub absolute_number: Option<u32>,
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
    let stem = Path::new(filename).file_stem()?.to_str()?;
    let normalized = stem
        .chars()
        .map(|character| match character {
            '.' | '_' | '(' | ')' | '[' | ']' => ' ',
            character => character,
        })
        .collect::<String>();
    let words: Vec<&str> = normalized.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let year_index = words.iter().position(|word| {
        word.len() == 4
            && word.chars().all(|character| character.is_ascii_digit())
            && word
                .parse::<i32>()
                .is_ok_and(|year| (1900..=2099).contains(&year))
    });
    let (title_words, suffix_words, production_year) = match year_index {
        Some(index) if index > 0 => (
            &words[..index],
            &words[index + 1..],
            words[index].parse::<i32>().ok(),
        ),
        _ => (&words[..], &[][..], None),
    };
    let title = title_words.join(" ");
    if title.is_empty() {
        return None;
    }
    let edition_name = parse_edition_name(suffix_words);
    let quality_label = parse_quality_label(suffix_words);
    let display_title = edition_name
        .as_ref()
        .map(|edition| format!("{title} ({edition})"))
        .unwrap_or(title);
    Some(ParsedMovieFilename {
        sort_title: display_title.to_lowercase(),
        title: display_title,
        production_year,
        edition_name,
        quality_label,
    })
}

fn parse_edition_name(words: &[&str]) -> Option<String> {
    let lowered = words
        .iter()
        .map(|word| word.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if lowered
        .windows(2)
        .any(|window| matches!(window, [first, second] if (first == "director" || first == "directors" || first == "director's") && *second == "cut"))
    {
        return Some("Director's Cut".to_owned());
    }
    if lowered
        .windows(2)
        .any(|window| matches!(window, [first, second] if *first == "extended" && *second == "cut"))
    {
        return Some("Extended Cut".to_owned());
    }
    [
        ("unrated", "Unrated"),
        ("theatrical", "Theatrical"),
        ("ultimate", "Ultimate"),
        ("final", "Final"),
        ("special", "Special"),
        ("remastered", "Remastered"),
    ]
    .iter()
    .find_map(|(token, label)| {
        lowered
            .iter()
            .any(|word| word == token)
            .then_some((*label).to_owned())
    })
}

fn parse_quality_label(words: &[&str]) -> Option<String> {
    words.iter().find_map(|word| {
        let normalized = word.to_ascii_lowercase();
        match normalized.as_str() {
            "4k" | "uhd" | "2160p" => Some(if normalized == "4k" || normalized == "uhd" {
                "4K".to_owned()
            } else {
                "2160p".to_owned()
            }),
            "1080p" | "720p" | "576p" | "480p" => Some(normalized),
            _ => None,
        }
    })
}

pub fn parse_episode_filename(filename: &str) -> Option<ParsedEpisodeFilename> {
    let stem = Path::new(filename).file_stem()?.to_str()?;
    let bytes = stem.as_bytes();
    let mut marker = None;
    for start in 0..bytes.len() {
        if matches!(bytes[start], b's' | b'S') {
            let (season_end, season) = ascii_number(bytes, start + 1);
            if let Some(season) = season
                && bytes
                    .get(season_end)
                    .is_some_and(|value| matches!(value, b'e' | b'E'))
            {
                let (episode_end, episode) = ascii_number(bytes, season_end + 1);
                if let Some(episode) = episode {
                    marker = Some((start, episode_end, season, episode));
                    break;
                }
            }
        }
        if bytes[start].is_ascii_digit() {
            let (season_end, season) = ascii_number(bytes, start);
            if let Some(season) = season
                && bytes
                    .get(season_end)
                    .is_some_and(|value| matches!(value, b'x' | b'X'))
            {
                let (episode_end, episode) = ascii_number(bytes, season_end + 1);
                if let Some(episode) = episode {
                    marker = Some((start, episode_end, season, episode));
                    break;
                }
            }
        }
    }
    let (start, end, season, episode) = marker?;
    let title = clean_hierarchy_title(&format!("{} {}", &stem[..start], &stem[end..]));
    let title = if title.is_empty() {
        format!("Episode {episode:02}")
    } else {
        title
    };
    Some(ParsedEpisodeFilename {
        sort_title: title.to_lowercase(),
        title,
        season,
        episode,
        absolute_number: None,
    })
}

fn ascii_number(bytes: &[u8], start: usize) -> (usize, Option<u32>) {
    let mut end = start;
    while bytes.get(end).is_some_and(u8::is_ascii_digit) {
        end += 1;
    }
    if end == start {
        return (start, None);
    }
    let value = std::str::from_utf8(&bytes[start..end])
        .ok()
        .and_then(|value| value.parse::<u32>().ok());
    (end, value)
}

fn clean_hierarchy_title(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '.' | '_' | '-' | '(' | ')' | '[' | ']' => ' ',
            character => character,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn season_directory_number(components: &[&str]) -> Option<u32> {
    components
        .get(components.len().saturating_sub(2))
        .and_then(|value| parse_season_directory_name(value))
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
    digits.parse::<u32>().ok()
}

pub(crate) async fn collect_movie_files(root: &Path) -> Result<Vec<PathBuf>, ScannerError> {
    let mut files = Vec::new();
    let mut entries = fs::read_dir(root)
        .await
        .map_err(|source| ScannerError::Io {
            path: root.to_owned(),
            source,
        })?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| ScannerError::Io {
            path: root.to_owned(),
            source,
        })?
    {
        let entry_path = entry.path();
        let file_type = entry.file_type().await.map_err(|source| ScannerError::Io {
            path: entry_path.clone(),
            source,
        })?;
        if file_type.is_file() && is_supported_movie_file(&entry_path) {
            files.push(entry_path);
        } else if file_type.is_dir() {
            let mut children =
                fs::read_dir(&entry_path)
                    .await
                    .map_err(|source| ScannerError::Io {
                        path: entry_path.clone(),
                        source,
                    })?;
            while let Some(child) =
                children
                    .next_entry()
                    .await
                    .map_err(|source| ScannerError::Io {
                        path: entry_path.clone(),
                        source,
                    })?
            {
                let child_path = child.path();
                if child
                    .file_type()
                    .await
                    .map_err(|source| ScannerError::Io {
                        path: child_path.clone(),
                        source,
                    })?
                    .is_file()
                    && is_supported_movie_file(&child_path)
                {
                    files.push(child_path);
                }
            }
        }
    }
    files.sort();
    Ok(files)
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
