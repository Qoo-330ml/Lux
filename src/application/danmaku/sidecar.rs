//! Sidecar file IO: validation-first atomic writes, bounded reads and
//! fingerprints.

use std::{fmt, path::Path};

use sha2::{Digest, Sha256};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};
use uuid::Uuid;

use super::{
    DanmakuService, DanmakuServiceError,
    path::safe_danmaku_source_paths,
    xml::{DanmakuXmlError, MAX_DANMAKU_XML_BYTES, validate_danmaku_xml},
};
use crate::storage::StoredDanmakuSource;

pub async fn atomic_write_danmaku_xml(
    target: &Path,
    bytes: &[u8],
    overwrite: bool,
) -> Result<(), DanmakuWriteError> {
    validate_danmaku_xml(bytes).map_err(DanmakuWriteError::InvalidXml)?;
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let target_type = fs::symlink_metadata(target).await;
    match target_type {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(DanmakuWriteError::SymlinkTarget);
        }
        Ok(_) if !overwrite => return Err(DanmakuWriteError::AlreadyExists),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(DanmakuWriteError::Io(error.to_string())),
        Ok(_) => {}
    }

    let temporary = parent.join(format!(".lux-{}.danmaku.xml.tmp", Uuid::now_v7()));
    let result = async {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(|error| DanmakuWriteError::Io(error.to_string()))?;
        file.write_all(bytes)
            .await
            .map_err(|error| DanmakuWriteError::Io(error.to_string()))?;
        file.sync_all()
            .await
            .map_err(|error| DanmakuWriteError::Io(error.to_string()))?;
        drop(file);

        if !overwrite {
            match fs::symlink_metadata(target).await {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(DanmakuWriteError::SymlinkTarget);
                }
                Ok(_) => return Err(DanmakuWriteError::AlreadyExists),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(DanmakuWriteError::Io(error.to_string())),
            }
        } else if fs::symlink_metadata(target)
            .await
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(DanmakuWriteError::SymlinkTarget);
        }

        fs::rename(&temporary, target)
            .await
            .map_err(|error| DanmakuWriteError::Io(error.to_string()))?;
        let directory = fs::File::open(parent)
            .await
            .map_err(|error| DanmakuWriteError::Io(error.to_string()))?;
        directory
            .sync_all()
            .await
            .map_err(|error| DanmakuWriteError::Io(error.to_string()))?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}

#[derive(Debug, Eq, PartialEq)]
pub enum DanmakuWriteError {
    AlreadyExists,
    SymlinkTarget,
    InvalidXml(DanmakuXmlError),
    Io(String),
}

impl fmt::Display for DanmakuWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists => formatter.write_str("danmaku XML already exists"),
            Self::SymlinkTarget => formatter.write_str("danmaku XML target cannot be a symlink"),
            Self::InvalidXml(error) => error.fmt(formatter),
            Self::Io(message) => write!(formatter, "danmaku XML IO error: {message}"),
        }
    }
}

impl std::error::Error for DanmakuWriteError {}

pub(crate) async fn read_danmaku_sidecar_from_root(
    root_path: &Path,
    relative_path: &Path,
) -> Result<Vec<u8>, ()> {
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(());
    }
    let root = fs::canonicalize(root_path).await.map_err(|_| ())?;
    let sidecar_path = fs::canonicalize(root.join(relative_path))
        .await
        .map_err(|_| ())?;
    if !sidecar_path.starts_with(&root)
        && !crate::application::scanner::symlink::path_is_allowed(&root, &sidecar_path)
    {
        return Err(());
    }
    if !fs::metadata(&sidecar_path).await.map_err(|_| ())?.is_file() {
        return Err(());
    }
    read_danmaku_file(&sidecar_path).await
}

pub(super) async fn read_danmaku_file(path: &Path) -> Result<Vec<u8>, ()> {
    let metadata = fs::metadata(path).await.map_err(|_| ())?;
    if metadata.len() > MAX_DANMAKU_XML_BYTES as u64 {
        return Err(());
    }
    let file = fs::File::open(path).await.map_err(|_| ())?;
    let mut bytes = Vec::new();
    file.take((MAX_DANMAKU_XML_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ())?;
    if bytes.len() > MAX_DANMAKU_XML_BYTES {
        return Err(());
    }
    Ok(bytes)
}

pub(crate) fn fingerprint(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

/// Which media source a playback read is allowed to use.
///
/// The two playback protocols differ only in this requirement; resolving the
/// source, the media-root checks and the XML validation are shared.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DanmakuSidecarScope {
    /// Any existing sidecar of the item's media source (Emby `/api/danmu/*`).
    AnySource,
    /// Only a source whose `danmaku_tracks` row is `READY` (Lux Web API).
    RegisteredTrack,
}

impl DanmakuService {
    pub async fn read_registered_sidecar_for_source(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<Vec<u8>>, DanmakuServiceError> {
        self.read_for_playback(item_id, source_id, DanmakuSidecarScope::RegisteredTrack)
            .await
    }

    /// Reads the sidecar a playback protocol may serve for an item.
    ///
    /// Both protocol handlers call this; only the [`DanmakuSidecarScope`] and
    /// their own authentication differ.
    pub async fn read_for_playback(
        &self,
        item_id: &str,
        source_id: Option<&str>,
        scope: DanmakuSidecarScope,
    ) -> Result<Option<Vec<u8>>, DanmakuServiceError> {
        let source = match scope {
            DanmakuSidecarScope::AnySource => {
                self.database
                    .find_danmaku_source_for_item(item_id, source_id)
                    .await?
            }
            DanmakuSidecarScope::RegisteredTrack => {
                self.database
                    .find_registered_danmaku_source_for_item(item_id, source_id)
                    .await?
            }
        };
        let Some(source) = source else {
            return Ok(None);
        };
        Ok(read_validated_sidecar(&source).await)
    }
}

async fn read_validated_sidecar(source: &StoredDanmakuSource) -> Option<Vec<u8>> {
    let Ok((_, target)) = safe_danmaku_source_paths(source).await else {
        return None;
    };
    let Ok(metadata) = fs::symlink_metadata(&target).await else {
        return None;
    };
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return None;
    }
    let Ok(bytes) = read_danmaku_file(&target).await else {
        return None;
    };
    if validate_danmaku_xml(&bytes).is_err() {
        return None;
    }
    Some(bytes)
}
