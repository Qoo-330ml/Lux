use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use tokio::fs;

use crate::domain::ids::{LibraryId, LibraryRootId};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LibraryKind {
    Movie,
    Series,
    Mixed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryRecord {
    pub id: LibraryId,
    pub name: String,
    pub kind: LibraryKind,
    pub is_enabled: bool,
    pub realtime_watch_enabled: bool,
    pub incremental_schedule: Option<String>,
    pub reconciliation_schedule: Option<String>,
    pub metadata_schedule: Option<String>,
    pub scan_concurrency: i64,
    pub probe_concurrency: i64,
    pub last_scan_at: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryRootRecord {
    pub id: LibraryRootId,
    pub library_id: LibraryId,
    pub canonical_path: PathBuf,
    pub display_path: PathBuf,
    pub is_available: bool,
    pub is_writable: bool,
    pub last_checked_at: i64,
    pub unavailable_since: Option<i64>,
    pub scan_cursor: Option<String>,
}

impl LibraryKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "MOVIE",
            Self::Series => "SERIES",
            Self::Mixed => "MIXED",
        }
    }
}

impl FromStr for LibraryKind {
    type Err = LibraryKindError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "MOVIE" => Ok(Self::Movie),
            "SERIES" => Ok(Self::Series),
            "MIXED" => Ok(Self::Mixed),
            _ => Err(LibraryKindError(value.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LibraryKindError(String);

impl fmt::Display for LibraryKindError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unsupported library kind: {}", self.0)
    }
}

impl std::error::Error for LibraryKindError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootPathInspection {
    pub canonical_path: PathBuf,
    pub is_available: bool,
    pub is_readable: bool,
    pub is_writable: bool,
}

#[derive(Debug)]
pub enum RootPathError {
    Unavailable { path: PathBuf, reason: String },
}

impl RootPathError {
    pub fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable { .. })
    }
}

impl fmt::Display for RootPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable { path, reason } => {
                write!(
                    formatter,
                    "root path {} is unavailable: {reason}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for RootPathError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RootOverlap {
    Exact,
    Nested,
    Disjoint,
}

pub fn classify_root_overlap(left: &Path, right: &Path) -> RootOverlap {
    if left == right {
        RootOverlap::Exact
    } else if left.starts_with(right) || right.starts_with(left) {
        RootOverlap::Nested
    } else {
        RootOverlap::Disjoint
    }
}

pub async fn inspect_root_path(path: &Path) -> Result<RootPathInspection, RootPathError> {
    let canonical_path =
        fs::canonicalize(path)
            .await
            .map_err(|error| RootPathError::Unavailable {
                path: path.to_owned(),
                reason: error.to_string(),
            })?;
    let metadata =
        fs::metadata(&canonical_path)
            .await
            .map_err(|error| RootPathError::Unavailable {
                path: canonical_path.clone(),
                reason: error.to_string(),
            })?;
    if !metadata.is_dir() {
        return Err(RootPathError::Unavailable {
            path: canonical_path,
            reason: "path is not a directory".to_owned(),
        });
    }

    let is_readable = fs::read_dir(&canonical_path).await.is_ok();
    if !is_readable {
        return Err(RootPathError::Unavailable {
            path: canonical_path,
            reason: "directory cannot be read".to_owned(),
        });
    }

    Ok(RootPathInspection {
        canonical_path,
        is_available: true,
        is_readable: true,
        is_writable: !metadata.permissions().readonly(),
    })
}
