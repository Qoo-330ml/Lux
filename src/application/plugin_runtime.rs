use std::{
    collections::HashSet,
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use zip::ZipArchive;

use super::plugin_protocol::{PluginManifest, PluginManifestError};

const MAX_PLUGIN_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PLUGIN_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PLUGIN_FILES: usize = 512;

#[derive(Clone, Debug)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub source_path: PathBuf,
    pub root_path: PathBuf,
    pub entrypoint: PathBuf,
    pub is_archive: bool,
}

#[derive(Clone, Debug)]
pub struct PluginDiscoveryFailure {
    pub source_path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct PluginCatalog {
    pub plugins: Vec<DiscoveredPlugin>,
    pub failures: Vec<PluginDiscoveryFailure>,
}

impl PluginCatalog {
    pub fn discover(plugin_dir: &Path) -> Self {
        let mut catalog = Self::default();
        let entries = match fs::read_dir(plugin_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return catalog,
            Err(error) => {
                catalog.failures.push(PluginDiscoveryFailure {
                    source_path: plugin_dir.to_owned(),
                    message: error.to_string(),
                });
                return catalog;
            }
        };
        let mut ids = HashSet::new();
        for entry in entries.flatten() {
            let source_path = entry.path();
            let result = if source_path.is_dir() {
                if source_path
                    .file_name()
                    .is_some_and(|name| name == ".extracted")
                {
                    continue;
                }
                discover_directory(&source_path)
            } else if source_path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
            {
                discover_archive(&source_path)
            } else {
                continue;
            };
            match result {
                Ok(plugin) if ids.insert(plugin.manifest.id.clone()) => {
                    catalog.plugins.push(plugin)
                }
                Ok(plugin) => catalog.failures.push(PluginDiscoveryFailure {
                    source_path,
                    message: format!("duplicate plugin id: {}", plugin.manifest.id),
                }),
                Err(error) => catalog.failures.push(PluginDiscoveryFailure {
                    source_path,
                    message: error.to_string(),
                }),
            }
        }
        catalog
            .plugins
            .sort_by(|left, right| left.manifest.id.cmp(&right.manifest.id));
        catalog
    }

    pub fn get(&self, plugin_id: &str) -> Option<&DiscoveredPlugin> {
        self.plugins
            .iter()
            .find(|plugin| plugin.manifest.id == plugin_id)
    }
}

#[derive(Debug)]
pub enum PluginDiscoveryError {
    Io(io::Error),
    Zip(zip::result::ZipError),
    InvalidManifest(PluginManifestError),
    InvalidPackage(String),
}

impl fmt::Display for PluginDiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "plugin IO error: {error}"),
            Self::Zip(error) => write!(formatter, "plugin ZIP error: {error}"),
            Self::InvalidManifest(error) => write!(formatter, "invalid plugin manifest: {error}"),
            Self::InvalidPackage(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PluginDiscoveryError {}

impl From<io::Error> for PluginDiscoveryError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<zip::result::ZipError> for PluginDiscoveryError {
    fn from(error: zip::result::ZipError) -> Self {
        Self::Zip(error)
    }
}

impl From<PluginManifestError> for PluginDiscoveryError {
    fn from(error: PluginManifestError) -> Self {
        Self::InvalidManifest(error)
    }
}

fn discover_directory(path: &Path) -> Result<DiscoveredPlugin, PluginDiscoveryError> {
    let manifest = read_manifest(&path.join("manifest.json"))?;
    let entrypoint = resolve_entrypoint(path, &manifest)?;
    verify_declared_files(path, &manifest)?;
    Ok(DiscoveredPlugin {
        manifest,
        source_path: path.to_owned(),
        root_path: path.to_owned(),
        entrypoint,
        is_archive: false,
    })
}

fn discover_archive(path: &Path) -> Result<DiscoveredPlugin, PluginDiscoveryError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_PLUGIN_ARCHIVE_BYTES {
        return Err(PluginDiscoveryError::InvalidPackage(
            "plugin archive is too large".to_owned(),
        ));
    }
    let mut archive = ZipArchive::new(File::open(path)?)?;
    if archive.len() > MAX_PLUGIN_FILES {
        return Err(PluginDiscoveryError::InvalidPackage(
            "plugin archive contains too many files".to_owned(),
        ));
    }
    let manifest = {
        let mut file = archive.by_name("manifest.json")?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        let value = serde_json::from_str(&contents)
            .map_err(|error| PluginDiscoveryError::InvalidPackage(error.to_string()))?;
        PluginManifest::from_value(value)?
    };
    let extracted_root = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".extracted")
        .join(format!("{}-{}", manifest.id, manifest.version));
    extract_archive(&mut archive, &extracted_root)?;
    let entrypoint = resolve_entrypoint(&extracted_root, &manifest)?;
    verify_declared_files(&extracted_root, &manifest)?;
    Ok(DiscoveredPlugin {
        manifest,
        source_path: path.to_owned(),
        root_path: extracted_root,
        entrypoint,
        is_archive: true,
    })
}

fn read_manifest(path: &Path) -> Result<PluginManifest, PluginDiscoveryError> {
    let contents = fs::read_to_string(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            PluginDiscoveryError::InvalidPackage("manifest.json is missing".to_owned())
        } else {
            PluginDiscoveryError::Io(error)
        }
    })?;
    let value = serde_json::from_str(&contents)
        .map_err(|error| PluginDiscoveryError::InvalidPackage(error.to_string()))?;
    PluginManifest::from_value(value).map_err(PluginDiscoveryError::InvalidManifest)
}

fn resolve_entrypoint(
    root: &Path,
    manifest: &PluginManifest,
) -> Result<PathBuf, PluginDiscoveryError> {
    let entrypoint = root.join(&manifest.runtime.entrypoint);
    if !entrypoint.is_file() {
        return Err(PluginDiscoveryError::InvalidPackage(format!(
            "plugin entrypoint does not exist: {}",
            manifest.runtime.entrypoint
        )));
    }
    Ok(entrypoint)
}

fn verify_declared_files(
    root: &Path,
    manifest: &PluginManifest,
) -> Result<(), PluginDiscoveryError> {
    for file in &manifest.files {
        let path = root.join(&file.path);
        if !path.is_file() {
            return Err(PluginDiscoveryError::InvalidPackage(format!(
                "declared plugin file does not exist: {}",
                file.path.display()
            )));
        }
        let actual = sha256_file(&path)?;
        if !actual.eq_ignore_ascii_case(&file.sha256) {
            return Err(PluginDiscoveryError::InvalidPackage(format!(
                "plugin file hash mismatch: {}",
                file.path.display()
            )));
        }
    }
    Ok(())
}

fn extract_archive(
    archive: &mut ZipArchive<File>,
    destination: &Path,
) -> Result<(), PluginDiscoveryError> {
    fs::create_dir_all(destination)?;
    let mut total_size = 0_u64;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let enclosed_name = file.enclosed_name().ok_or_else(|| {
            PluginDiscoveryError::InvalidPackage(
                "plugin archive contains an unsafe path".to_owned(),
            )
        })?;
        let target = destination.join(enclosed_name);
        if file.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        total_size = total_size.saturating_add(file.size());
        if total_size > MAX_PLUGIN_UNCOMPRESSED_BYTES {
            return Err(PluginDiscoveryError::InvalidPackage(
                "plugin archive expands beyond the size limit".to_owned(),
            ));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = File::create(&target)?;
        io::copy(&mut file, &mut output)?;
        output.flush()?;
        #[cfg(unix)]
        if target.starts_with(destination.join("binaries")) {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = output.metadata()?.permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&target, permissions)?;
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, PluginDiscoveryError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
