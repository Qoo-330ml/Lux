use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex as StdMutex, OnceLock},
    time::{Duration, Instant, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

const INTERNAL_WRITE_MARKER_TTL: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FileStamp {
    pub size: u64,
    pub modified: Option<(u64, u32)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Marker {
    expires_at: Instant,
    expected_stamp: Option<FileStamp>,
    expected_content: Option<[u8; 32]>,
}

static MARKERS: OnceLock<StdMutex<HashMap<PathBuf, Marker>>> = OnceLock::new();

fn registry() -> &'static StdMutex<HashMap<PathBuf, Marker>> {
    MARKERS.get_or_init(|| StdMutex::new(HashMap::new()))
}

pub(crate) fn register(path: &Path) {
    let now = Instant::now();
    let mut markers = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    markers.retain(|_, marker| marker.expires_at > now);
    markers.insert(
        path.to_owned(),
        Marker {
            expires_at: now + INTERNAL_WRITE_MARKER_TTL,
            expected_stamp: None,
            expected_content: None,
        },
    );
}

pub(crate) fn finalize(path: &Path, expected_stamp: Option<FileStamp>, content: &[u8]) {
    let now = Instant::now();
    let mut markers = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    markers.retain(|_, marker| marker.expires_at > now);
    if let Some(marker) = markers.get_mut(path) {
        marker.expected_stamp = expected_stamp;
        marker.expected_content = Some(content_fingerprint(content));
    }
}

pub(crate) async fn should_suppress(path: &Path) -> bool {
    let now = Instant::now();
    let marker = {
        let mut markers = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        markers.retain(|_, marker| marker.expires_at > now);
        markers.get(path).copied()
    };
    let Some(marker) = marker else {
        return false;
    };
    let Some(expected_stamp) = marker.expected_stamp else {
        return true;
    };
    if file_stamp(path).await.ok().flatten() == Some(expected_stamp)
        && marker.expected_content == read_content_fingerprint(path).await.ok().flatten()
    {
        return true;
    }

    let mut markers = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if markers.get(path).copied() == Some(marker) {
        markers.remove(path);
    }
    false
}

fn content_fingerprint(content: &[u8]) -> [u8; 32] {
    Sha256::digest(content).into()
}

async fn read_content_fingerprint(path: &Path) -> std::io::Result<Option<[u8; 32]>> {
    match tokio::fs::read(path).await {
        Ok(content) => Ok(Some(content_fingerprint(&content))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) async fn file_stamp(path: &Path) -> std::io::Result<Option<FileStamp>> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Ok(None);
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| (value.as_secs(), value.subsec_nanos()));
    Ok(Some(FileStamp {
        size: metadata.len(),
        modified,
    }))
}

#[cfg(test)]
mod tests {
    use super::{finalize, register, should_suppress};

    #[tokio::test]
    async fn external_content_with_restored_stamp_is_not_suppressed() {
        let root = tempfile::tempdir().expect("temporary root");
        let target = root.path().join("movie.nfo");
        tokio::fs::write(&target, b"internal").await.expect("write");
        let modified = std::fs::metadata(&target)
            .expect("metadata")
            .modified()
            .expect("mtime");
        register(&target);
        finalize(
            &target,
            super::file_stamp(&target).await.expect("stamp"),
            b"internal",
        );
        assert!(should_suppress(&target).await);
        tokio::fs::write(&target, b"external")
            .await
            .expect("external write");
        std::fs::File::options()
            .write(true)
            .open(&target)
            .expect("open")
            .set_modified(modified)
            .expect("restore mtime");
        assert!(!should_suppress(&target).await);
    }

    #[tokio::test]
    async fn temporary_marker_is_suppressed_and_external_update_invalidates_target() {
        let root = tempfile::tempdir().expect("temporary root");
        let temporary = root.path().join(".lux-write.tmp");
        register(&temporary);
        assert!(should_suppress(&temporary).await);

        let target = root.path().join("movie.nfo");
        tokio::fs::write(&target, b"internal")
            .await
            .expect("write target");
        register(&target);
        finalize(
            &target,
            super::file_stamp(&target)
                .await
                .expect("stamp")
                .as_ref()
                .copied(),
            b"internal",
        );
        assert!(should_suppress(&target).await);
        tokio::fs::write(&target, b"external change")
            .await
            .expect("external update");
        assert!(!should_suppress(&target).await);
    }
}
