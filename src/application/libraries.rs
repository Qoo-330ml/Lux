use std::{fmt, path::Path};

use crate::{
    domain::ids::{LibraryId, LibraryRootId},
    library::{
        LibraryKind, LibraryRecord, LibraryRootRecord, RootOverlap, RootPathError,
        classify_root_overlap, inspect_root_path,
    },
    storage::{
        Database, NewLibrary, NewLibraryRoot, StorageError, StoredLibrary, StoredLibraryRoot,
    },
};

const DEFAULT_SCAN_CONCURRENCY: i64 = 2;
const DEFAULT_PROBE_CONCURRENCY: i64 = 1;

#[derive(Clone)]
pub struct LibraryService {
    database: Database,
}

impl LibraryService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn create_library(
        &self,
        name: &str,
        kind: LibraryKind,
        realtime_watch_enabled: bool,
    ) -> Result<LibraryRecord, LibraryServiceError> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 128 {
            return Err(LibraryServiceError::InvalidName);
        }
        let id = LibraryId::new();
        self.database
            .insert_library(NewLibrary {
                id: &id.to_string(),
                name,
                kind: kind.as_str(),
                realtime_watch_enabled,
                incremental_schedule: None,
                reconciliation_schedule: None,
                metadata_schedule: None,
                scan_concurrency: DEFAULT_SCAN_CONCURRENCY,
                probe_concurrency: DEFAULT_PROBE_CONCURRENCY,
            })
            .await?;
        let stored = self
            .database
            .find_library(&id.to_string())
            .await?
            .ok_or(LibraryServiceError::LibraryNotFound)?;
        stored_library(stored)
    }

    pub async fn list_libraries(&self) -> Result<Vec<LibraryView>, LibraryServiceError> {
        let libraries = self.database.list_libraries().await?;
        let mut views = Vec::with_capacity(libraries.len());
        for library in libraries {
            let id = library.id.clone();
            let library = stored_library(library)?;
            let roots = self
                .database
                .list_library_roots(&id)
                .await?
                .into_iter()
                .map(stored_library_root)
                .collect::<Result<Vec<_>, _>>()?;
            views.push(LibraryView { library, roots });
        }
        Ok(views)
    }

    pub async fn add_root(
        &self,
        library_id: LibraryId,
        display_path: &str,
    ) -> Result<AddRootResult, LibraryServiceError> {
        let library_id_text = library_id.to_string();
        if self
            .database
            .find_library(&library_id_text)
            .await?
            .is_none()
        {
            return Err(LibraryServiceError::LibraryNotFound);
        }

        let inspection = inspect_root_path(Path::new(display_path)).await?;
        let existing_roots = self.database.list_all_library_roots().await?;
        let mut warnings = Vec::new();
        for existing in existing_roots {
            let existing_path = Path::new(&existing.canonical_path);
            let overlap = classify_root_overlap(&inspection.canonical_path, existing_path);
            if overlap == RootOverlap::Disjoint {
                continue;
            }
            if existing.library_id == library_id_text {
                return Err(match overlap {
                    RootOverlap::Exact => LibraryServiceError::DuplicateRoot,
                    RootOverlap::Nested => LibraryServiceError::OverlappingRoot,
                    RootOverlap::Disjoint => unreachable!(),
                });
            }
            warnings.push(LibraryWarningCode::CrossLibraryOverlap);
        }

        if !inspection.is_writable {
            warnings.push(LibraryWarningCode::PathNotWritable);
        }

        let id = LibraryRootId::new();
        self.database
            .insert_library_root(NewLibraryRoot {
                id: &id.to_string(),
                library_id: &library_id_text,
                canonical_path: &inspection.canonical_path.to_string_lossy(),
                display_path,
                is_available: inspection.is_available,
                is_writable: inspection.is_writable,
            })
            .await?;
        let root = self
            .database
            .find_library_root(&id.to_string())
            .await?
            .ok_or(LibraryServiceError::RootNotFoundAfterInsert)
            .and_then(stored_library_root)?;
        Ok(AddRootResult { root, warnings })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryView {
    pub library: LibraryRecord,
    pub roots: Vec<LibraryRootRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AddRootResult {
    pub root: LibraryRootRecord,
    pub warnings: Vec<LibraryWarningCode>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LibraryWarningCode {
    CrossLibraryOverlap,
    PathNotWritable,
}

impl LibraryWarningCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CrossLibraryOverlap => "CROSS_LIBRARY_OVERLAP",
            Self::PathNotWritable => "LIBRARY_PATH_NOT_WRITABLE",
        }
    }
}

#[derive(Debug)]
pub enum LibraryServiceError {
    InvalidName,
    InvalidLibraryId(String),
    InvalidRootId(String),
    InvalidKind(String),
    LibraryNotFound,
    RootNotFoundAfterInsert,
    DuplicateRoot,
    OverlappingRoot,
    Path(RootPathError),
    Storage(StorageError),
}

impl fmt::Display for LibraryServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => formatter.write_str("library name must be 1-128 characters"),
            Self::InvalidLibraryId(error) => write!(formatter, "invalid library ID: {error}"),
            Self::InvalidRootId(error) => write!(formatter, "invalid library root ID: {error}"),
            Self::InvalidKind(error) => write!(formatter, "invalid library kind: {error}"),
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::RootNotFoundAfterInsert => {
                formatter.write_str("library root was inserted but could not be read back")
            }
            Self::DuplicateRoot => formatter.write_str("the root path is already in this library"),
            Self::OverlappingRoot => {
                formatter.write_str("the root path overlaps another root in this library")
            }
            Self::Path(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for LibraryServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Path(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::InvalidName
            | Self::InvalidLibraryId(_)
            | Self::InvalidRootId(_)
            | Self::InvalidKind(_)
            | Self::LibraryNotFound
            | Self::RootNotFoundAfterInsert
            | Self::DuplicateRoot
            | Self::OverlappingRoot => None,
        }
    }
}

impl From<RootPathError> for LibraryServiceError {
    fn from(error: RootPathError) -> Self {
        Self::Path(error)
    }
}

impl From<StorageError> for LibraryServiceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

fn stored_library(stored: StoredLibrary) -> Result<LibraryRecord, LibraryServiceError> {
    let id = stored
        .id
        .parse()
        .map_err(|error: uuid::Error| LibraryServiceError::InvalidLibraryId(error.to_string()))?;
    let kind = stored
        .kind
        .parse()
        .map_err(|error: crate::library::LibraryKindError| {
            LibraryServiceError::InvalidKind(error.to_string())
        })?;
    Ok(LibraryRecord {
        id,
        name: stored.name,
        kind,
        is_enabled: stored.is_enabled,
        realtime_watch_enabled: stored.realtime_watch_enabled,
        incremental_schedule: stored.incremental_schedule,
        reconciliation_schedule: stored.reconciliation_schedule,
        metadata_schedule: stored.metadata_schedule,
        scan_concurrency: stored.scan_concurrency,
        probe_concurrency: stored.probe_concurrency,
        last_scan_at: stored.last_scan_at,
    })
}

fn stored_library_root(
    stored: StoredLibraryRoot,
) -> Result<LibraryRootRecord, LibraryServiceError> {
    let id = stored
        .id
        .parse()
        .map_err(|error: uuid::Error| LibraryServiceError::InvalidRootId(error.to_string()))?;
    let library_id = stored
        .library_id
        .parse()
        .map_err(|error: uuid::Error| LibraryServiceError::InvalidLibraryId(error.to_string()))?;
    Ok(LibraryRootRecord {
        id,
        library_id,
        canonical_path: stored.canonical_path.into(),
        display_path: stored.display_path.into(),
        is_available: stored.is_available,
        is_writable: stored.is_writable,
        last_checked_at: stored.last_checked_at,
        unavailable_since: stored.unavailable_since,
        scan_cursor: stored.scan_cursor,
    })
}
