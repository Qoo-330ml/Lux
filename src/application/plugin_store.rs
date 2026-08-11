use std::{fmt, io::Write, path::PathBuf, time::Duration};

use reqwest::{Client, Url};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::fs;
use uuid::Uuid;

use crate::network::client_builder_from_env_or;

pub const DEFAULT_PLUGIN_STORE_URL: &str = "https://github.com/Qoo-330ml/Lux-plugins";
const PLUGIN_STORE_URL_FILE: &str = "plugin_store_url";
const MAX_STORE_URL_LENGTH: usize = 2048;
const MAX_INDEX_BYTES: usize = 2 * 1024 * 1024;
const MAX_PACKAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_PLUGIN_COUNT: usize = 100;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct PluginStoreIndex {
    #[serde(rename = "formatVersion", alias = "format_version")]
    pub format_version: u32,
    pub plugins: Vec<PluginStoreEntry>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct PluginStorePackage {
    pub platform: String,
    pub arch: String,
    #[serde(rename = "url", alias = "package")]
    pub package_url: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct PluginStoreEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub category: String,
    pub version: String,
    #[serde(default)]
    pub runtime: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub packages: Vec<PluginStorePackage>,
    #[serde(default, rename = "package")]
    pub package_url: String,
    #[serde(default)]
    pub sha256: String,
}

#[derive(Debug)]
pub enum PluginStoreError {
    InvalidSource,
    InvalidCatalog,
    InvalidPackage,
    UnsupportedPlatform,
    Http(reqwest::Error),
    Io(std::io::Error),
}

impl fmt::Display for PluginStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSource => formatter.write_str("plugin store source is invalid"),
            Self::InvalidCatalog => formatter.write_str("plugin store catalog is invalid"),
            Self::InvalidPackage => formatter.write_str("plugin package is invalid"),
            Self::UnsupportedPlatform => {
                formatter.write_str("plugin package is unavailable for this platform")
            }
            Self::Http(_) => formatter.write_str("plugin store request failed"),
            Self::Io(_) => formatter.write_str("plugin store file operation failed"),
        }
    }
}

impl std::error::Error for PluginStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::InvalidSource
            | Self::InvalidCatalog
            | Self::InvalidPackage
            | Self::UnsupportedPlatform => None,
        }
    }
}

#[derive(Clone)]
pub struct PluginStore {
    config_dir: PathBuf,
    client: Client,
}

impl PluginStore {
    pub fn new(config_dir: PathBuf, proxy_url: Option<String>) -> Result<Self, PluginStoreError> {
        let client = client_builder_from_env_or(proxy_url.as_deref())
            .map_err(|_| PluginStoreError::InvalidSource)?
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent("Lux plugin store")
            .build()
            .map_err(PluginStoreError::Http)?;
        Ok(Self { config_dir, client })
    }

    pub async fn source(&self) -> String {
        fs::read_to_string(self.config_dir.join(PLUGIN_STORE_URL_FILE))
            .await
            .ok()
            .and_then(|value| validate_store_source(&value).ok().map(str::to_owned))
            .unwrap_or_else(|| DEFAULT_PLUGIN_STORE_URL.to_owned())
    }

    pub async fn save_source(&self, source: &str) -> Result<String, PluginStoreError> {
        let source = validate_store_source(source)?;
        fs::create_dir_all(&self.config_dir)
            .await
            .map_err(PluginStoreError::Io)?;
        let path = self.config_dir.join(PLUGIN_STORE_URL_FILE);
        let temporary = self
            .config_dir
            .join(format!(".plugin-store-{}.tmp", Uuid::now_v7()));
        let source = source.to_string();
        let stored_source = source.clone();
        tokio::task::spawn_blocking(move || {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(stored_source.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            std::fs::rename(temporary, path)
        })
        .await
        .map_err(|error| PluginStoreError::Io(std::io::Error::other(error.to_string())))?
        .map_err(PluginStoreError::Io)?;
        Ok(source)
    }

    pub async fn fetch_catalog(&self) -> Result<PluginStoreIndex, PluginStoreError> {
        let source = self.source().await;
        let catalog_url = catalog_url(&source)?;
        let response = self
            .client
            .get(catalog_url.clone())
            .send()
            .await
            .map_err(PluginStoreError::Http)?
            .error_for_status()
            .map_err(PluginStoreError::Http)?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_INDEX_BYTES as u64)
        {
            return Err(PluginStoreError::InvalidCatalog);
        }
        let bytes =
            read_limited_body(response, MAX_INDEX_BYTES, PluginStoreError::InvalidCatalog).await?;
        let mut index: PluginStoreIndex =
            serde_json::from_slice(&bytes).map_err(|_| PluginStoreError::InvalidCatalog)?;
        validate_catalog(&mut index, &catalog_url)?;
        Ok(index)
    }

    pub async fn download_package(
        &self,
        entry: &PluginStoreEntry,
    ) -> Result<PathBuf, PluginStoreError> {
        let (package_url, expected) = select_package(entry)?;
        let response = self
            .client
            .get(package_url)
            .send()
            .await
            .map_err(PluginStoreError::Http)?
            .error_for_status()
            .map_err(PluginStoreError::Http)?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_PACKAGE_BYTES as u64)
        {
            return Err(PluginStoreError::InvalidPackage);
        }
        let bytes = read_limited_body(
            response,
            MAX_PACKAGE_BYTES,
            PluginStoreError::InvalidPackage,
        )
        .await?;
        let expected = expected.to_ascii_lowercase();
        let temporary = self
            .config_dir
            .join(format!(".lux-plugin-{}.zip", Uuid::now_v7()));
        let bytes = bytes.to_vec();
        let path = temporary.clone();
        let result = match tokio::task::spawn_blocking(move || {
            let actual = format!("{:x}", Sha256::digest(&bytes));
            if actual != expected {
                return Err(PluginStoreError::InvalidPackage);
            }
            let mut file = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .map_err(PluginStoreError::Io)?;
            file.write_all(&bytes).map_err(PluginStoreError::Io)?;
            file.sync_all().map_err(PluginStoreError::Io)?;
            Ok(path)
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                let _ = fs::remove_file(&temporary).await;
                return Err(PluginStoreError::Io(std::io::Error::other(
                    error.to_string(),
                )));
            }
        };
        match result {
            Ok(path) => Ok(path),
            Err(error) => {
                let _ = fs::remove_file(&temporary).await;
                Err(error)
            }
        }
    }

    pub fn default_index() -> Result<PluginStoreIndex, PluginStoreError> {
        let mut index: PluginStoreIndex = serde_json::from_str(DEFAULT_PLUGIN_INDEX_JSON)
            .map_err(|_| PluginStoreError::InvalidCatalog)?;
        let base = catalog_url(DEFAULT_PLUGIN_STORE_URL)?;
        validate_catalog(&mut index, &base)?;
        Ok(index)
    }
}

async fn read_limited_body(
    mut response: reqwest::Response,
    maximum: usize,
    too_large: PluginStoreError,
) -> Result<Vec<u8>, PluginStoreError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(PluginStoreError::Http)? {
        if bytes.len().saturating_add(chunk.len()) > maximum {
            return Err(too_large);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn select_package(entry: &PluginStoreEntry) -> Result<(&str, &str), PluginStoreError> {
    let (platform, arch) = current_platform_and_arch();
    if let Some(package) = entry
        .packages
        .iter()
        .find(|package| package.platform == platform && package.arch == arch)
    {
        return Ok((&package.package_url, &package.sha256));
    }
    if let Some(package) = entry
        .packages
        .iter()
        .find(|package| package.platform == "*" && package.arch == "*")
    {
        return Ok((&package.package_url, &package.sha256));
    }
    if entry.packages.is_empty() && !entry.package_url.is_empty() && !entry.sha256.is_empty() {
        return Ok((&entry.package_url, &entry.sha256));
    }
    Err(PluginStoreError::UnsupportedPlatform)
}

fn current_platform_and_arch() -> (&'static str, &'static str) {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        "linux" => "linux",
        _ => std::env::consts::OS,
    };
    let arch = match (platform, std::env::consts::ARCH) {
        ("darwin", "aarch64") => "arm64",
        (_, "aarch64") => "aarch64",
        (_, "x86_64") => "x86_64",
        (_, value) => value,
    };
    (platform, arch)
}

pub fn validate_store_source(source: &str) -> Result<&str, PluginStoreError> {
    let source = source.trim();
    if source.is_empty()
        || source.len() > MAX_STORE_URL_LENGTH
        || source.chars().any(char::is_control)
    {
        return Err(PluginStoreError::InvalidSource);
    }
    let url = Url::parse(source).map_err(|_| PluginStoreError::InvalidSource)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(PluginStoreError::InvalidSource);
    }
    Ok(source)
}

pub fn catalog_url(source: &str) -> Result<Url, PluginStoreError> {
    let source = validate_store_source(source)?;
    let url = Url::parse(source).map_err(|_| PluginStoreError::InvalidSource)?;
    let segments = url
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if url.host_str() == Some("github.com") && segments.len() == 2 {
        return Url::parse(&format!(
            "https://raw.githubusercontent.com/{}/{}/main/index.json",
            segments[0], segments[1]
        ))
        .map_err(|_| PluginStoreError::InvalidSource);
    }
    Ok(url)
}

fn validate_catalog(
    index: &mut PluginStoreIndex,
    catalog_url: &Url,
) -> Result<(), PluginStoreError> {
    if index.format_version != 1 || index.plugins.len() > MAX_PLUGIN_COUNT {
        return Err(PluginStoreError::InvalidCatalog);
    }
    let mut ids = std::collections::HashSet::new();
    for entry in &mut index.plugins {
        if entry.id.is_empty()
            || entry.id.len() > 128
            || entry.id.chars().any(|character| {
                !(character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_'))
            })
            || !ids.insert(entry.id.clone())
            || entry.version.is_empty()
            || entry.version.len() > 128
            || entry.version.contains('/')
        {
            return Err(PluginStoreError::InvalidCatalog);
        }
        if entry.packages.is_empty() {
            if entry.package_url.is_empty()
                || entry.sha256.len() != 64
                || !entry
                    .sha256
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
            {
                return Err(PluginStoreError::InvalidCatalog);
            }
            entry.packages.push(PluginStorePackage {
                platform: "*".to_owned(),
                arch: "*".to_owned(),
                package_url: entry.package_url.clone(),
                sha256: entry.sha256.clone(),
            });
        }
        let mut package_targets = std::collections::HashSet::new();
        for package in &mut entry.packages {
            if package.platform.is_empty()
                || package.platform.len() > 32
                || package.platform.chars().any(|character| {
                    !(character.is_ascii_alphanumeric() || matches!(character, '*' | '-' | '_'))
                })
                || package.arch.is_empty()
                || package.arch.len() > 32
                || package.arch.chars().any(|character| {
                    !(character.is_ascii_alphanumeric() || matches!(character, '*' | '-' | '_'))
                })
                || !package_targets.insert((package.platform.clone(), package.arch.clone()))
                || package.sha256.len() != 64
                || !package
                    .sha256
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
            {
                return Err(PluginStoreError::InvalidCatalog);
            }
            let package_url = catalog_url
                .join(&package.package_url)
                .map_err(|_| PluginStoreError::InvalidCatalog)?;
            if package_url.scheme() != "https"
                || package_url.host_str().is_none()
                || !package_url.username().is_empty()
                || package_url.password().is_some()
                || package_url.fragment().is_some()
            {
                return Err(PluginStoreError::InvalidCatalog);
            }
            package.package_url = package_url.to_string();
        }
        if entry.package_url.is_empty() {
            entry.package_url = entry.packages[0].package_url.clone();
            entry.sha256 = entry.packages[0].sha256.clone();
        } else {
            entry.package_url = entry.packages[0].package_url.clone();
        }
    }
    Ok(())
}

const DEFAULT_PLUGIN_INDEX_JSON: &str = r#"
{
  "formatVersion": 1,
  "plugins": [
    {"id":"org.lux.tmdb","name":"TMDb 元数据插件","description":"从 TMDb 提供 Emby 风格电影、剧集和图片元数据。","category":"SCRAPER","version":"0.1.5","runtime":"process","capabilities":["metadata.search","metadata.get","metadata.images","metadata.credits","metadata.externalIds","metadata.trailers"],"packages":[{"platform":"linux","arch":"aarch64","url":"https://github.com/Qoo-330ml/Lux-plugins/releases/download/build-1/org.lux.tmdb-0.1.5-linux-aarch64.zip","sha256":"00e2b91bf51a32e98debee8049c460d6da0a8f5e4c91af025f88b4045303c9e6"},{"platform":"linux","arch":"x86_64","url":"https://github.com/Qoo-330ml/Lux-plugins/releases/download/build-1/org.lux.tmdb-0.1.5-linux-x86_64.zip","sha256":"0babd6b44507b54986ce8aa0dfcbc9cb918ca0aed8ff41039a3fede6bc97ef62"}]},
    {"id":"org.lux.strm-media-info","name":"strm媒体信息提取","description":"使用 ffprobe 提取媒体信息，并使用 ffmpeg 补全 STRM 缩略图。","category":"MEDIA","version":"0.2.0","runtime":"process","capabilities":["media.probe"],"packages":[{"platform":"linux","arch":"aarch64","url":"https://github.com/Qoo-330ml/Lux-plugins/releases/download/build-1/org.lux.strm-media-info-0.2.0-linux-aarch64.zip","sha256":"042e19c26090818a127cad3d5a757396dd77679eb3cc1240b92b780faadd38e5"},{"platform":"linux","arch":"x86_64","url":"https://github.com/Qoo-330ml/Lux-plugins/releases/download/build-1/org.lux.strm-media-info-0.2.0-linux-x86_64.zip","sha256":"ba9c051b804ee20bbf4f24eeb8a3aeccb7e5e9922f8d7a469c19e4fe41a199b2"}]},
    {"id":"org.lux.ip-hiofd","name":"IP归属地查询增强","description":"通过 Hiofd 查询公网 IP 的归属地信息。","category":"NETWORK","version":"0.1.0","runtime":"process","capabilities":["ip.location"],"packages":[{"platform":"linux","arch":"aarch64","url":"https://github.com/Qoo-330ml/Lux-plugins/releases/download/build-1/org.lux.ip-hiofd-0.1.0-linux-aarch64.zip","sha256":"1cc18a886d87477debaa48c1947ff324183b158a3014a682a6b28f3129ec1f28"},{"platform":"linux","arch":"x86_64","url":"https://github.com/Qoo-330ml/Lux-plugins/releases/download/build-1/org.lux.ip-hiofd-0.1.0-linux-x86_64.zip","sha256":"d07929bdfcc486fb2704d70be3da9a690858da6fb7ea053fd4571483127b5d9a"}]},
    {"id":"org.lux.qoo-ip138","name":"ip138 IP归属地查询","description":"通过 ipshudi.com 查询公网 IP 的归属地信息。","category":"NETWORK","version":"0.1.0","runtime":"process","capabilities":["ip.location"],"packages":[{"platform":"linux","arch":"aarch64","url":"https://github.com/Qoo-330ml/Lux-plugins/releases/download/build-1/org.lux.qoo-ip138-0.1.0-linux-aarch64.zip","sha256":"f38791fd6a8798bfad195ce94a08fa5891ce96b24cd1eda7449ee694e7894d73"},{"platform":"linux","arch":"x86_64","url":"https://github.com/Qoo-330ml/Lux-plugins/releases/download/build-1/org.lux.qoo-ip138-0.1.0-linux-x86_64.zip","sha256":"508e512fa0719df5f19233db8c3b4113672a975dd96325e03042bf0955377cbb"}]}
  ]
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_github_repository_sources_and_resolves_index() {
        let source = validate_store_source(DEFAULT_PLUGIN_STORE_URL).expect("valid source");
        assert_eq!(source, DEFAULT_PLUGIN_STORE_URL);
        assert_eq!(
            catalog_url(source).expect("catalog URL").as_str(),
            "https://raw.githubusercontent.com/Qoo-330ml/Lux-plugins/main/index.json"
        );
    }

    #[test]
    fn rejects_unsafe_store_sources() {
        for source in [
            "http://example.com/index.json",
            "https://user:password@example.com/index.json",
            "https://example.com/index.json#fragment",
            "https://example.com/index.json\nX-Injected: true",
        ] {
            assert!(validate_store_source(source).is_err(), "{source}");
        }
    }

    #[test]
    fn validates_catalog_entries_and_resolves_relative_packages() {
        let mut index = PluginStoreIndex {
            format_version: 1,
            plugins: vec![PluginStoreEntry {
                id: "org.lux.example".to_owned(),
                name: "Example".to_owned(),
                description: String::new(),
                category: "UTILITY".to_owned(),
                version: "1.0.0".to_owned(),
                runtime: "process".to_owned(),
                capabilities: Vec::new(),
                packages: Vec::new(),
                package_url: "packages/example.zip".to_owned(),
                sha256: "a".repeat(64),
            }],
        };
        validate_catalog(
            &mut index,
            &Url::parse("https://example.com/store/index.json").expect("base URL"),
        )
        .expect("valid catalog");
        assert_eq!(
            index.plugins[0].package_url,
            "https://example.com/store/packages/example.zip"
        );
        assert_eq!(index.plugins[0].packages[0].platform, "*");
    }

    #[test]
    fn embedded_default_catalog_matches_the_published_store_shape() {
        let index = PluginStore::default_index().expect("embedded catalog should be valid");
        assert_eq!(index.format_version, 1);
        assert_eq!(index.plugins.len(), 4);
        assert_eq!(index.plugins[0].id, "org.lux.tmdb");
        assert_eq!(index.plugins[0].packages.len(), 2);
    }

    #[test]
    fn selects_the_package_for_the_running_platform_and_architecture() {
        let (platform, arch) = current_platform_and_arch();
        let entry = PluginStoreEntry {
            id: "org.lux.example".to_owned(),
            name: "Example".to_owned(),
            description: String::new(),
            category: "UTILITY".to_owned(),
            version: "1.0.0".to_owned(),
            runtime: "process".to_owned(),
            capabilities: Vec::new(),
            packages: vec![
                PluginStorePackage {
                    platform: "linux".to_owned(),
                    arch: "x86_64".to_owned(),
                    package_url: "https://example.com/x86.zip".to_owned(),
                    sha256: "a".repeat(64),
                },
                PluginStorePackage {
                    platform: platform.to_owned(),
                    arch: arch.to_owned(),
                    package_url: "https://example.com/current.zip".to_owned(),
                    sha256: "b".repeat(64),
                },
            ],
            package_url: String::new(),
            sha256: String::new(),
        };
        assert_eq!(
            select_package(&entry).expect("matching package").0,
            "https://example.com/current.zip"
        );
    }

    #[tokio::test]
    async fn persists_and_reads_a_custom_store_source() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = PluginStore::new(directory.path().to_owned(), None).expect("store client");
        assert_eq!(store.source().await, DEFAULT_PLUGIN_STORE_URL);
        store
            .save_source("https://example.com/lux/index.json")
            .await
            .expect("save store source");
        assert_eq!(store.source().await, "https://example.com/lux/index.json");
    }
}
