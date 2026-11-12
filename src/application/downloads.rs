use std::{
    fmt,
    path::{Path, PathBuf},
};

use tokio::{fs, io::AsyncReadExt};

use crate::{
    application::strm_target::{
        StrmLocalPathError, StrmTargetKind, canonical_local_strm_target, classify_strm_target,
    },
    storage::{Database, StorageError, StoredPlaybackSource},
};

const MAX_STRM_BYTES: u64 = 8 * 1024;

#[derive(Clone)]
pub struct DownloadService {
    database: Database,
}

/// A file Lux serves itself. URL and SMB/FTP `.strm` targets never become artifacts: the API
/// layer redirects them to the client instead of relaying their bytes.
#[derive(Debug)]
pub struct DownloadArtifact {
    path: PathBuf,
    file_name: String,
}

impl DownloadArtifact {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }
}

impl DownloadService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn prepare(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<DownloadArtifact, DownloadError> {
        let source = match source_id {
            Some(source_id) => {
                self.database
                    .find_download_source_by_id(item_id, source_id)
                    .await?
            }
            None => self.database.find_download_source(item_id).await?,
        }
        .ok_or(DownloadError::ItemNotFound)?;
        self.prepare_resolved_source(
            &source.source_kind,
            &source.root_path,
            &source.relative_path,
        )
        .await
    }

    pub(crate) async fn prepare_authorized_source(
        &self,
        source: &StoredPlaybackSource,
    ) -> Result<DownloadArtifact, DownloadError> {
        self.prepare_resolved_source(
            &source.source_kind,
            &source.root_path,
            &source.relative_path,
        )
        .await
    }

    async fn prepare_resolved_source(
        &self,
        source_kind: &str,
        root_path: &str,
        relative_path: &str,
    ) -> Result<DownloadArtifact, DownloadError> {
        let root = fs::canonicalize(root_path).await?;
        let media_path = fs::canonicalize(root.join(relative_path)).await?;
        if media_path == root
            || (!media_path.starts_with(&root)
                && !crate::application::scanner::symlink::path_is_allowed(&root, &media_path))
        {
            return Err(DownloadError::PathOutsideRoot(media_path));
        }
        let metadata = fs::metadata(&media_path).await?;
        if !metadata.is_file() {
            return Err(DownloadError::ItemNotFound);
        }
        let file_name = media_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| DownloadError::InvalidFileName(media_path.clone()))?
            .to_owned();
        if source_kind == "STRM_URL" {
            let strm_target = read_strm_target(&media_path).await?;
            match classify_strm_target(&strm_target).kind {
                StrmTargetKind::Path => {
                    let target_path =
                        canonical_local_strm_target(root_path, relative_path, &strm_target)
                            .await
                            .map_err(|error| match error {
                                StrmLocalPathError::Missing => DownloadError::ItemNotFound,
                                StrmLocalPathError::Forbidden => {
                                    DownloadError::PathOutsideRoot(media_path.clone())
                                }
                            })?;
                    if target_path
                        .extension()
                        .and_then(|value| value.to_str())
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"))
                    {
                        return Err(DownloadError::UnsupportedStrmTarget);
                    }
                    let target_metadata = fs::metadata(&target_path).await?;
                    if !target_metadata.is_file() {
                        return Err(DownloadError::ItemNotFound);
                    }
                    let file_name = target_path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .ok_or_else(|| DownloadError::InvalidFileName(target_path.clone()))?
                        .to_owned();
                    Ok(DownloadArtifact {
                        path: target_path,
                        file_name,
                    })
                }
                // Redirected by the API layer before it reaches this point.
                StrmTargetKind::Url
                | StrmTargetKind::Smb
                | StrmTargetKind::Ftp
                | StrmTargetKind::Empty
                | StrmTargetKind::Unsupported => Err(DownloadError::UnsupportedStrmTarget),
            }
        } else {
            Ok(DownloadArtifact {
                path: media_path,
                file_name,
            })
        }
    }
}

async fn read_strm_target(path: &Path) -> Result<String, DownloadError> {
    let file = fs::File::open(path).await?;
    let mut contents = Vec::new();
    file.take(MAX_STRM_BYTES + 1)
        .read_to_end(&mut contents)
        .await?;
    if contents.len() as u64 > MAX_STRM_BYTES {
        return Err(DownloadError::UnsupportedStrmTarget);
    }
    let contents = String::from_utf8(contents).map_err(|_| DownloadError::UnsupportedStrmTarget)?;
    contents
        .lines()
        .map(|line| line.trim_start_matches('\u{feff}').trim())
        .find(|line| !line.is_empty())
        .map(str::to_owned)
        .ok_or(DownloadError::UnsupportedStrmTarget)
}

pub(crate) fn is_matching_sidecar(selected_name: &str, candidate_name: &str) -> bool {
    let Some(selected_stem) = selected_name.rsplit_once('.').map(|(stem, _)| stem) else {
        return false;
    };
    let Some((candidate_stem, extension)) = candidate_name.rsplit_once('.') else {
        return false;
    };
    if extension.eq_ignore_ascii_case("xml") {
        return candidate_stem.eq_ignore_ascii_case(selected_stem);
    }
    if extension.eq_ignore_ascii_case("json")
        && candidate_stem.eq_ignore_ascii_case(&format!("{selected_stem}-mediainfo"))
    {
        return true;
    }
    if !is_sidecar_extension(extension) {
        return false;
    }
    candidate_stem.eq_ignore_ascii_case(selected_stem)
        || candidate_stem
            .to_ascii_lowercase()
            .starts_with(&format!("{}.", selected_stem.to_ascii_lowercase()))
}

fn is_sidecar_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "srt"
            | "ass"
            | "ssa"
            | "vtt"
            | "sub"
            | "sup"
            | "idx"
            | "nfo"
            | "jpg"
            | "jpeg"
            | "png"
            | "webp"
    )
}

#[derive(Debug)]
pub enum DownloadError {
    ItemNotFound,
    InvalidFileName(PathBuf),
    PathOutsideRoot(PathBuf),
    Io(std::io::Error),
    Storage(StorageError),
    UnsupportedStrmTarget,
}

impl fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ItemNotFound => formatter.write_str("download item not found"),
            Self::InvalidFileName(path) => {
                write!(formatter, "invalid media filename '{}'", path.display())
            }
            Self::PathOutsideRoot(path) => write!(
                formatter,
                "download path is outside root: {}",
                path.display()
            ),
            Self::Io(error) => write!(formatter, "download file operation failed: {error}"),
            Self::Storage(error) => error.fmt(formatter),
            Self::UnsupportedStrmTarget => formatter.write_str("STRM target is not downloadable"),
        }
    }
}

impl std::error::Error for DownloadError {}

impl From<std::io::Error> for DownloadError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<StorageError> for DownloadError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[cfg(test)]
mod tests {
    use super::is_matching_sidecar;

    #[test]
    fn only_selected_source_sidecars_are_downloaded() {
        assert!(is_matching_sidecar(
            "二毛 (2019) - 2160p - H.265 - AAC - test.mkv",
            "二毛 (2019) - 2160p - H.265 - AAC - test.zh.ass",
        ));
        assert!(is_matching_sidecar(
            "Example.Movie.2024.mkv",
            "Example.Movie.2024.xml",
        ));
        assert!(is_matching_sidecar(
            "Example.Movie.2024.strm",
            "Example.Movie.2024-mediainfo.json",
        ));
        assert!(!is_matching_sidecar(
            "Example.Movie.2024.mkv",
            "Example.Movie.2024.Extended.xml",
        ));
        assert!(!is_matching_sidecar(
            "二毛 (2019) - 2160p - H.265 - AAC - test.mkv",
            "二毛 (2019) - 1080p - H.264 - AAC.mkv",
        ));
    }

    #[test]
    fn media_files_are_never_classified_as_sidecars() {
        assert!(!is_matching_sidecar("Movie.mkv", "movie.MKV"));
        assert!(!is_matching_sidecar("Movie.strm", "movie.STRM"));
    }
}
