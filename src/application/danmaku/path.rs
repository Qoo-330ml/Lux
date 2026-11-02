//! Sidecar path derivation and media-root path safety.
//!
//! Every path that reaches the filesystem goes through this module: the sidecar
//! name is derived from the media file name, and both the media file and its
//! sidecar are canonicalised and checked against the library root (including
//! the allowed-symlink rule used by the scanner).

use std::{
    fmt,
    path::{Component, Path, PathBuf},
};

use tokio::fs;

use crate::storage::StoredDanmakuSource;

pub fn danmaku_sidecar_path(video_path: &Path) -> Result<PathBuf, DanmakuPathError> {
    if video_path.is_absolute()
        || video_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(DanmakuPathError::OutsideMediaRoot);
    }
    let file_name = video_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or(DanmakuPathError::InvalidFileName)?;
    let stem = video_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or(DanmakuPathError::InvalidFileName)?;
    if file_name == stem {
        return Err(DanmakuPathError::InvalidFileName);
    }
    let mut sidecar = video_path.to_path_buf();
    sidecar.set_file_name(format!("{stem}.xml"));
    Ok(sidecar)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DanmakuPathError {
    OutsideMediaRoot,
    InvalidFileName,
}

impl fmt::Display for DanmakuPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutsideMediaRoot => formatter.write_str("danmaku path leaves the media root"),
            Self::InvalidFileName => formatter.write_str("danmaku media filename is invalid"),
        }
    }
}

impl std::error::Error for DanmakuPathError {}

/// Resolves the canonical media path and the sidecar path for a stored source.
///
/// Returns `Err(())` for any path that escapes the library root; callers map
/// that to a protocol-level "not available" answer.
pub(super) async fn safe_danmaku_source_paths(
    source: &StoredDanmakuSource,
) -> Result<(PathBuf, PathBuf), ()> {
    let relative = Path::new(&source.relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(());
    }
    let root = fs::canonicalize(&source.root_path).await.map_err(|_| ())?;
    let media_path = fs::canonicalize(root.join(relative))
        .await
        .map_err(|_| ())?;
    if !media_path.starts_with(&root)
        && !crate::application::scanner::symlink::path_is_allowed(&root, &media_path)
    {
        return Err(());
    }
    let sidecar_relative = danmaku_sidecar_path(relative).map_err(|_| ())?;
    let parent = media_path.parent().ok_or(())?.to_path_buf();
    if !parent.starts_with(&root)
        && !crate::application::scanner::symlink::path_is_allowed(&root, &parent)
    {
        return Err(());
    }
    let sidecar_name = sidecar_relative.file_name().ok_or(())?.to_owned();
    let sidecar_path = parent.join(sidecar_name);
    if let Ok(canonical_sidecar) = fs::canonicalize(&sidecar_path).await {
        if !canonical_sidecar.starts_with(&root)
            && !crate::application::scanner::symlink::path_is_allowed(&root, &canonical_sidecar)
        {
            return Err(());
        }
    }
    Ok((media_path, sidecar_path))
}
