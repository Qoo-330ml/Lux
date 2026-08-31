use std::{
    fmt,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use reqwest::{Method, Response, Url, header::RANGE, redirect::Policy};
use tokio::{fs, io::AsyncReadExt};

use crate::{
    application::remote_url_policy::{RemoteMediaUrlError, validate_and_resolve_remote_media_url},
    application::strm_target::{
        StrmLocalPathError, StrmTargetKind, canonical_local_strm_target, classify_strm_target,
    },
    network::{NetworkProxyError, client_builder_from_env_or},
    storage::{Database, StorageError, StoredPlaybackSource},
};

const MAX_STRM_BYTES: u64 = 8 * 1024;

#[derive(Clone)]
pub struct DownloadService {
    database: Database,
    proxy_url: Option<String>,
}

#[derive(Debug)]
pub enum DownloadArtifact {
    LocalFile {
        path: PathBuf,
        file_name: String,
    },
    Remote {
        url: Url,
        address: SocketAddr,
        file_name: String,
    },
}

impl DownloadArtifact {
    pub fn local_path(&self) -> Option<&Path> {
        match self {
            Self::LocalFile { path, .. } => Some(path),
            Self::Remote { .. } => None,
        }
    }

    pub fn file_name(&self) -> &str {
        match self {
            Self::LocalFile { file_name, .. } | Self::Remote { file_name, .. } => file_name,
        }
    }
}

impl DownloadService {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            proxy_url: None,
        }
    }

    pub fn new_with_proxy(
        database: Database,
        proxy_url: Option<String>,
    ) -> Result<Self, DownloadError> {
        client_builder_from_env_or(proxy_url.as_deref())
            .map_err(DownloadError::ProxyConfiguration)?
            .build()
            .map_err(|error| DownloadError::ClientBuild(error.to_string()))?;
        Ok(Self {
            database,
            proxy_url,
        })
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
        if !media_path.starts_with(&root) || media_path == root {
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
                    Ok(DownloadArtifact::LocalFile {
                        path: target_path,
                        file_name,
                    })
                }
                StrmTargetKind::Url => {
                    let (url, address) = validate_and_resolve_remote_media_url(&strm_target)
                        .await
                        .map_err(DownloadError::RemoteUrl)?;
                    let file_name = remote_file_name(&url).unwrap_or(file_name);
                    Ok(DownloadArtifact::Remote {
                        url,
                        address,
                        file_name,
                    })
                }
                StrmTargetKind::Smb
                | StrmTargetKind::Ftp
                | StrmTargetKind::Empty
                | StrmTargetKind::Unsupported => Err(DownloadError::UnsupportedStrmTarget),
            }
        } else {
            Ok(DownloadArtifact::LocalFile {
                path: media_path,
                file_name,
            })
        }
    }

    pub async fn fetch_remote(
        &self,
        artifact: &DownloadArtifact,
        method: &Method,
        range: Option<&str>,
    ) -> Result<Response, DownloadError> {
        let DownloadArtifact::Remote { url, address, .. } = artifact else {
            return Err(DownloadError::RemoteRequest);
        };
        let host = url
            .host_str()
            .ok_or(DownloadError::RemoteUrl(RemoteMediaUrlError::Invalid))?;
        let client = client_builder_from_env_or(self.proxy_url.as_deref())
            .map_err(DownloadError::ProxyConfiguration)?
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .resolve(host, *address)
            .build()
            .map_err(|error| DownloadError::ClientBuild(error.to_string()))?;
        let mut request = match *method {
            Method::GET => client.get(url.clone()),
            Method::HEAD => client.head(url.clone()),
            _ => return Err(DownloadError::RemoteRequest),
        };
        if let Some(range) = range {
            request = request.header(RANGE, range);
        }
        request
            .send()
            .await
            .map_err(|_| DownloadError::RemoteRequest)
    }
}

async fn read_strm_target(path: &Path) -> Result<String, DownloadError> {
    let file = fs::File::open(path).await?;
    let mut contents = Vec::new();
    file.take(MAX_STRM_BYTES + 1)
        .read_to_end(&mut contents)
        .await?;
    if contents.len() as u64 > MAX_STRM_BYTES {
        return Err(DownloadError::RemoteUrl(RemoteMediaUrlError::Invalid));
    }
    let contents = String::from_utf8(contents)
        .map_err(|_| DownloadError::RemoteUrl(RemoteMediaUrlError::Invalid))?;
    contents
        .lines()
        .map(|line| line.trim_start_matches('\u{feff}').trim())
        .find(|line| !line.is_empty())
        .map(str::to_owned)
        .ok_or(DownloadError::RemoteUrl(RemoteMediaUrlError::Invalid))
}

fn remote_file_name(url: &Url) -> Option<String> {
    url.path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .filter(|segment| {
            *segment != "."
                && *segment != ".."
                && segment.chars().all(|character| {
                    !character.is_control() && !matches!(character, '/' | '\\' | '"')
                })
        })
        .map(str::to_owned)
}

pub(crate) fn is_matching_sidecar(selected_name: &str, candidate_name: &str) -> bool {
    if selected_name.eq_ignore_ascii_case(candidate_name) {
        return true;
    }
    let Some(selected_stem) = selected_name.rsplit_once('.').map(|(stem, _)| stem) else {
        return false;
    };
    let Some((candidate_stem, extension)) = candidate_name.rsplit_once('.') else {
        return false;
    };
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
    ProxyConfiguration(NetworkProxyError),
    ClientBuild(String),
    RemoteUrl(RemoteMediaUrlError),
    UnsupportedStrmTarget,
    RemoteRequest,
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
            Self::ProxyConfiguration(_) => {
                formatter.write_str("download proxy configuration is invalid")
            }
            Self::ClientBuild(_) => formatter.write_str("download client could not be created"),
            Self::RemoteUrl(error) => match error {
                RemoteMediaUrlError::Invalid => formatter.write_str("STRM remote URL is invalid"),
                RemoteMediaUrlError::BlockedHost => {
                    formatter.write_str("STRM remote host is blocked")
                }
                RemoteMediaUrlError::ResolutionFailed => {
                    formatter.write_str("STRM remote host could not be resolved")
                }
            },
            Self::UnsupportedStrmTarget => formatter.write_str("STRM target is not downloadable"),
            Self::RemoteRequest => formatter.write_str("STRM remote request failed"),
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

impl From<NetworkProxyError> for DownloadError {
    fn from(error: NetworkProxyError) -> Self {
        Self::ProxyConfiguration(error)
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
        assert!(!is_matching_sidecar(
            "二毛 (2019) - 2160p - H.265 - AAC - test.mkv",
            "二毛 (2019) - 1080p - H.264 - AAC.mkv",
        ));
    }
}
