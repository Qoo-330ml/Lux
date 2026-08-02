use std::{
    fmt,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use tokio::fs;
use uuid::Uuid;

use crate::{
    domain::ids::{FilesystemEntryId, ItemId, LibraryId, SourceId},
    storage::{Database, NewFilesystemEntry, NewMediaItem, NewMediaSource, StorageError},
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
            if !root.is_available {
                continue;
            }
            let root_path = PathBuf::from(root.canonical_path);
            let files = collect_movie_files(&root_path).await?;
            for path in files {
                let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let Some(parsed_name) = parse_movie_filename(file_name) else {
                    continue;
                };
                let relative_path = path
                    .strip_prefix(&root_path)
                    .map_err(|error| ScannerError::InvalidRelativePath(error.to_string()))?;
                let relative_path = relative_path
                    .to_str()
                    .ok_or(ScannerError::NonUtf8Path)?
                    .to_owned();
                let metadata = fs::metadata(&path)
                    .await
                    .map_err(|source| ScannerError::Io {
                        path: path.clone(),
                        source,
                    })?;
                let size = i64::try_from(metadata.len())
                    .map_err(|_| ScannerError::FileSizeOverflow(path.clone()))?;
                let modified_at = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
                    .unwrap_or(0);
                report.discovered_files += 1;
                if let Some(existing_entry) = self
                    .database
                    .find_filesystem_entry(&root.id, &relative_path)
                    .await?
                {
                    if existing_entry.size == size && existing_entry.modified_at == modified_at {
                        report.skipped_files += 1;
                        continue;
                    }
                    self.database
                        .update_filesystem_entry(&existing_entry.id, size, modified_at, &generation)
                        .await?;
                    self.database
                        .reset_media_probe_for_filesystem_entry(&existing_entry.id, size)
                        .await?;
                    report.changed_files += 1;
                    continue;
                }
                let entry_id = FilesystemEntryId::new();
                let entry_id_text = entry_id.to_string();
                self.database
                    .insert_filesystem_entry(NewFilesystemEntry {
                        id: &entry_id_text,
                        library_root_id: &root.id,
                        relative_path: &relative_path,
                        entry_kind: "FILE",
                        size,
                        modified_at,
                        last_seen_generation: &generation,
                    })
                    .await?;

                let existing_item = self
                    .database
                    .find_media_item(
                        &library_id_text,
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
                            library_id: &library_id_text,
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
                        filesystem_entry_id: &entry_id_text,
                        container: &container,
                        size,
                        is_default: created_item,
                    })
                    .await?;
                report.created_sources += 1;
                if created_item {
                    report.created_items += 1;
                }
            }
        }
        Ok(report)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanReport {
    pub discovered_files: usize,
    pub created_items: usize,
    pub created_sources: usize,
    pub changed_files: usize,
    pub skipped_files: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedMovieFilename {
    pub title: String,
    pub sort_title: String,
    pub production_year: Option<i32>,
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
    let (title_words, production_year) = match year_index {
        Some(index) if index > 0 => (&words[..index], words[index].parse::<i32>().ok()),
        _ => (&words[..], None),
    };
    let title = title_words.join(" ");
    if title.is_empty() {
        return None;
    }
    Some(ParsedMovieFilename {
        sort_title: title.to_lowercase(),
        title,
        production_year,
    })
}

async fn collect_movie_files(root: &Path) -> Result<Vec<PathBuf>, ScannerError> {
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

fn is_supported_movie_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "mkv" | "mp4"))
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
