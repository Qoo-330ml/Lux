use std::path::Path;

use tokio::fs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryPage {
    pub path: String,
    pub parent_path: Option<String>,
    pub directories: Vec<DirectoryEntry>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryBrowserError {
    InvalidPath,
    NotDirectory,
    Unavailable,
}

pub async fn list_directories(
    path: &Path,
    offset: usize,
    limit: usize,
) -> Result<DirectoryPage, DirectoryBrowserError> {
    if !path.is_absolute() || limit == 0 {
        return Err(DirectoryBrowserError::InvalidPath);
    }

    let canonical = fs::canonicalize(path)
        .await
        .map_err(|_| DirectoryBrowserError::Unavailable)?;
    let canonical_path = canonical
        .to_str()
        .ok_or(DirectoryBrowserError::InvalidPath)?
        .to_owned();
    let metadata = fs::metadata(&canonical)
        .await
        .map_err(|_| DirectoryBrowserError::Unavailable)?;
    if !metadata.is_dir() {
        return Err(DirectoryBrowserError::NotDirectory);
    }

    let parent_path = canonical.parent().and_then(Path::to_str).map(str::to_owned);
    let mut reader = fs::read_dir(&canonical)
        .await
        .map_err(|_| DirectoryBrowserError::Unavailable)?;
    let mut skipped = 0_usize;
    let mut directories = Vec::with_capacity(limit);
    let mut has_more = false;

    while let Some(entry) = reader
        .next_entry()
        .await
        .map_err(|_| DirectoryBrowserError::Unavailable)?
    {
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(path) = entry.path().to_str().map(str::to_owned) else {
            continue;
        };
        if skipped < offset {
            skipped += 1;
            continue;
        }
        if directories.len() == limit {
            has_more = true;
            break;
        }
        directories.push(DirectoryEntry { name, path });
    }

    Ok(DirectoryPage {
        path: canonical_path,
        parent_path,
        directories,
        has_more,
    })
}
