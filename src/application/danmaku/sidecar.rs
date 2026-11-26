//! Sidecar file IO: validation-first atomic writes, bounded reads and
//! fingerprints.

use std::{
    collections::VecDeque,
    fmt,
    path::{Path, PathBuf},
    sync::Mutex,
    time::SystemTime,
};

use sha2::{Digest, Sha256};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};
use uuid::Uuid;

use super::{
    DanmakuService, DanmakuServiceError,
    path::safe_danmaku_source_paths,
    xml::{
        DanmakuXmlError, MAX_DANMAKU_XML_BYTES, validate_danmaku_xml, validate_danmaku_xml_file,
    },
};
use crate::storage::StoredDanmakuSource;

/// Bounded LRU of recent sidecar XML validation verdicts so the playback info endpoint followed by
/// the raw XML stream endpoint validates an unchanged sidecar once instead of twice.
const DANMAKU_VALIDATION_CACHE_CAPACITY: usize = 128;

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

/// Open handle to a validated danmaku sidecar file ready for streaming playback responses.
pub struct DanmakuPlaybackStream {
    pub file: fs::File,
    pub content_length: u64,
}

#[derive(Clone, Eq, PartialEq)]
struct DanmakuValidationKey {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    inode: u64,
}

impl DanmakuValidationKey {
    fn from_metadata(path: PathBuf, metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        let inode = {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        };
        Self {
            path,
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            inode,
        }
    }
}

#[derive(Default)]
pub(super) struct DanmakuValidationCache {
    entries: Mutex<VecDeque<(DanmakuValidationKey, bool)>>,
    #[cfg(test)]
    validation_passes: std::sync::atomic::AtomicUsize,
}

impl DanmakuValidationCache {
    fn get(&self, key: &DanmakuValidationKey) -> Option<bool> {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let index = entries.iter().position(|(cached, _)| cached == key)?;
        let entry = entries.remove(index)?;
        let valid = entry.1;
        entries.push_back(entry);
        Some(valid)
    }

    fn insert(&self, key: DanmakuValidationKey, valid: bool) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(index) = entries.iter().position(|(cached, _)| cached == &key) {
            entries.remove(index);
        } else if entries.len() >= DANMAKU_VALIDATION_CACHE_CAPACITY {
            entries.pop_front();
        }
        entries.push_back((key, valid));
    }
}

impl DanmakuService {
    /// Checks whether a valid playback danmaku sidecar exists without buffering it in memory.
    pub async fn has_for_playback(
        &self,
        item_id: &str,
        source_id: Option<&str>,
        scope: DanmakuSidecarScope,
    ) -> Result<bool, DanmakuServiceError> {
        let Some(source) = self.find_playback_source(item_id, source_id, scope).await? else {
            return Ok(false);
        };
        Ok(self
            .resolve_validated_sidecar_target(&source)
            .await
            .is_some())
    }

    /// Opens a validated playback danmaku sidecar for streaming without buffering the full XML.
    pub async fn open_for_playback(
        &self,
        item_id: &str,
        source_id: Option<&str>,
        scope: DanmakuSidecarScope,
    ) -> Result<Option<DanmakuPlaybackStream>, DanmakuServiceError> {
        let Some(source) = self.find_playback_source(item_id, source_id, scope).await? else {
            return Ok(None);
        };
        let Some((target, key)) = self.resolve_validated_sidecar_target(&source).await else {
            return Ok(None);
        };
        let Ok(link_meta) = fs::symlink_metadata(&target).await else {
            return Ok(None);
        };
        if link_meta.file_type().is_symlink() || !link_meta.file_type().is_file() {
            return Ok(None);
        }
        let Ok(file) = fs::File::open(&target).await else {
            return Ok(None);
        };
        let Ok(file_meta) = file.metadata().await else {
            return Ok(None);
        };
        let opened_key = DanmakuValidationKey::from_metadata(target, &file_meta);
        if opened_key != key {
            // The sidecar changed between validation and open; re-validate once before serving.
            let Some((_, rechecked_key)) = self.resolve_validated_sidecar_target(&source).await
            else {
                return Ok(None);
            };
            if rechecked_key != opened_key {
                return Ok(None);
            }
        }
        Ok(Some(DanmakuPlaybackStream {
            file,
            content_length: file_meta.len(),
        }))
    }

    /// Reads the sidecar a playback protocol may serve for an item.
    ///
    /// Both protocol handlers use [`Self::has_for_playback`] and [`Self::open_for_playback`] to
    /// avoid buffering and double-parsing; this helper stays available for callers needing owned
    /// bytes.
    pub async fn read_for_playback(
        &self,
        item_id: &str,
        source_id: Option<&str>,
        scope: DanmakuSidecarScope,
    ) -> Result<Option<Vec<u8>>, DanmakuServiceError> {
        let Some(stream) = self.open_for_playback(item_id, source_id, scope).await? else {
            return Ok(None);
        };
        if stream.content_length > MAX_DANMAKU_XML_BYTES as u64 {
            return Ok(None);
        }
        let mut bytes = Vec::with_capacity(stream.content_length as usize);
        if stream
            .file
            .take(stream.content_length)
            .read_to_end(&mut bytes)
            .await
            .is_err()
        {
            return Ok(None);
        }
        Ok(Some(bytes))
    }

    async fn resolve_validated_sidecar_target(
        &self,
        source: &StoredDanmakuSource,
    ) -> Option<(PathBuf, DanmakuValidationKey)> {
        let Ok((_, target)) =
            safe_danmaku_source_paths(&source.root_path, &source.relative_path).await
        else {
            return None;
        };
        let Ok(metadata) = fs::symlink_metadata(&target).await else {
            return None;
        };
        if metadata.file_type().is_symlink()
            || !metadata.file_type().is_file()
            || metadata.len() == 0
            || metadata.len() > MAX_DANMAKU_XML_BYTES as u64
        {
            return None;
        }
        let key = DanmakuValidationKey::from_metadata(target.clone(), &metadata);
        if let Some(valid) = self.validation_cache.get(&key) {
            return valid.then_some((target, key));
        }
        let validate_path = target.clone();
        let valid = tokio::task::spawn_blocking(move || validate_danmaku_xml_file(&validate_path))
            .await
            .ok()
            .is_some_and(|result| result.is_ok());
        #[cfg(test)]
        self.validation_cache
            .validation_passes
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.validation_cache.insert(key.clone(), valid);
        valid.then_some((target, key))
    }

    async fn find_playback_source(
        &self,
        item_id: &str,
        source_id: Option<&str>,
        scope: DanmakuSidecarScope,
    ) -> Result<Option<StoredDanmakuSource>, DanmakuServiceError> {
        Ok(match scope {
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
        })
    }

    #[cfg(test)]
    fn validation_pass_count(&self) -> usize {
        self.validation_cache
            .validation_passes
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn has_for_playback_followed_by_open_for_playback_validates_xml_only_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let (temp_dir, database) = crate::storage::test_database().await?;
        let media_root = temp_dir.path().join("media");
        tokio::fs::create_dir_all(&media_root).await?;
        tokio::fs::write(media_root.join("Episode.01.mkv"), b"video").await?;
        let valid_xml = br#"<i><d p="1.5,1,25,16777215,0,0,0,0">hello danmaku</d></i>"#;
        let sidecar_path = media_root.join("Episode.01.xml");
        tokio::fs::write(&sidecar_path, valid_xml).await?;

        sqlx::query(
            "INSERT INTO libraries (id, name, kind, is_enabled)
             VALUES ('danmaku-lib', 'Danmaku', 'MOVIE', 1)",
        )
        .execute(database.pool())
        .await?;
        let root_str = media_root.to_string_lossy().into_owned();
        sqlx::query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('danmaku-root', 'danmaku-lib', ?, ?, 1, 1)",
        )
        .bind(&root_str)
        .bind(&root_str)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation, is_missing
             ) VALUES ('danmaku-entry', 'danmaku-root', 'Episode.01.mkv', 'FILE', 5, 1, 'g1', 0)",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 has_available_source
             ) VALUES ('danmaku-item', 'danmaku-lib', 'MOVIE', 'Episode 01', 'episode 01',
                       'LOCAL_CONFIRMED', 1)",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_sources (id, item_id, source_kind, filesystem_entry_id, is_default)
             VALUES ('danmaku-source', 'danmaku-item', 'LOCAL_FILE', 'danmaku-entry', 1)",
        )
        .execute(database.pool())
        .await?;

        let service = DanmakuService::new(database);
        assert!(
            service
                .has_for_playback("danmaku-item", None, DanmakuSidecarScope::AnySource)
                .await?
        );
        let mut stream = service
            .open_for_playback("danmaku-item", None, DanmakuSidecarScope::AnySource)
            .await?
            .expect("danmaku stream should open");
        assert_eq!(stream.content_length, valid_xml.len() as u64);
        let mut streamed = Vec::new();
        stream.file.read_to_end(&mut streamed).await?;
        assert_eq!(streamed, valid_xml);
        assert_eq!(
            service.validation_pass_count(),
            1,
            "info followed by raw playback must not parse the same unchanged sidecar XML twice"
        );

        // Changing the sidecar invalidates the cached verdict and re-validates.
        tokio::fs::write(&sidecar_path, b"<html>not valid danmaku anymore</html>").await?;
        assert!(
            !service
                .has_for_playback("danmaku-item", None, DanmakuSidecarScope::AnySource)
                .await?
        );
        assert_eq!(service.validation_pass_count(), 2);
        Ok(())
    }
}
