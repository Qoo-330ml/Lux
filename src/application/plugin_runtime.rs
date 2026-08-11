use std::{
    collections::{HashMap, HashSet},
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, RwLock},
    time::timeout,
};
use uuid::Uuid;
use zip::ZipArchive;

use super::plugin_protocol::{PluginManifest, PluginManifestError, PluginRequest, PluginResponse};

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

const DEFAULT_PLUGIN_CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct PluginSupervisor {
    catalog: Arc<RwLock<PluginCatalog>>,
    processes: Arc<Mutex<std::collections::HashMap<String, Arc<Mutex<PluginProcess>>>>>,
    last_errors: Arc<Mutex<HashMap<String, String>>>,
    call_timeout: Duration,
    config_dir: Option<PathBuf>,
    network_proxy_url: Option<String>,
}

impl PluginSupervisor {
    pub fn new(catalog: PluginCatalog) -> Self {
        Self::new_with_shared_catalog(Arc::new(RwLock::new(catalog)))
    }

    pub fn new_with_shared_catalog(catalog: Arc<RwLock<PluginCatalog>>) -> Self {
        Self {
            catalog,
            processes: Arc::new(Mutex::new(std::collections::HashMap::new())),
            last_errors: Arc::new(Mutex::new(HashMap::new())),
            call_timeout: DEFAULT_PLUGIN_CALL_TIMEOUT,
            config_dir: None,
            network_proxy_url: None,
        }
    }

    pub fn with_call_timeout(mut self, call_timeout: Duration) -> Self {
        self.call_timeout = call_timeout;
        self
    }

    pub fn with_config_dir(mut self, config_dir: PathBuf) -> Self {
        self.config_dir = Some(config_dir);
        self
    }

    pub fn with_network_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.network_proxy_url = proxy_url;
        self
    }

    pub async fn catalog(&self) -> PluginCatalog {
        self.catalog.read().await.clone()
    }

    pub async fn call(
        &self,
        plugin_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, PluginRuntimeError> {
        let plugin = self
            .catalog
            .read()
            .await
            .get(plugin_id)
            .cloned()
            .ok_or_else(|| PluginRuntimeError::UnknownPlugin(plugin_id.to_owned()))?;
        let process = {
            let mut processes = self.processes.lock().await;
            if let Some(process) = processes.get(plugin_id) {
                process.clone()
            } else {
                let process = match spawn_process(
                    &plugin,
                    self.config_dir.as_deref(),
                    self.network_proxy_url.as_deref(),
                ) {
                    Ok(process) => Arc::new(Mutex::new(process)),
                    Err(error) => {
                        self.record_error(plugin_id, &error).await;
                        return Err(error);
                    }
                };
                processes.insert(plugin_id.to_owned(), process.clone());
                process
            }
        };
        let result = process
            .lock()
            .await
            .call(method, params, self.call_timeout)
            .await;
        if result.is_err() {
            self.processes.lock().await.remove(plugin_id);
            if let Err(error) = &result {
                self.record_error(plugin_id, error).await;
            }
        } else {
            self.last_errors.lock().await.remove(plugin_id);
        }
        result
    }

    /// Runs one request in a fresh plugin process.
    ///
    /// Media probing is intentionally isolated per request so the host can
    /// enforce a bounded ffprobe concurrency without serializing every probe
    /// behind one long-lived stdin/stdout session.
    pub async fn call_isolated(
        &self,
        plugin_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, PluginRuntimeError> {
        let plugin = self
            .catalog
            .read()
            .await
            .get(plugin_id)
            .cloned()
            .ok_or_else(|| PluginRuntimeError::UnknownPlugin(plugin_id.to_owned()))?;
        let mut process = match spawn_process(
            &plugin,
            self.config_dir.as_deref(),
            self.network_proxy_url.as_deref(),
        ) {
            Ok(process) => process,
            Err(error) => {
                self.record_error(plugin_id, &error).await;
                return Err(error);
            }
        };
        let result = process.call(method, params, self.call_timeout).await;
        if let Err(error) = &result {
            self.record_error(plugin_id, error).await;
        } else {
            self.last_errors.lock().await.remove(plugin_id);
        }
        result
    }

    pub async fn status(&self, plugin_id: &str) -> PluginRuntimeStatus {
        let running = self.processes.lock().await.contains_key(plugin_id);
        let last_error = self.last_errors.lock().await.get(plugin_id).cloned();
        PluginRuntimeStatus {
            running,
            last_error,
        }
    }

    pub async fn stop_all(&self) {
        let processes = std::mem::take(&mut *self.processes.lock().await);
        for process in processes.into_values() {
            let mut process = process.lock().await;
            let _ = process.child.kill().await;
        }
    }

    pub async fn stop(&self, plugin_id: &str) {
        let Some(process) = self.processes.lock().await.remove(plugin_id) else {
            return;
        };
        let mut process = process.lock().await;
        let _ = process.child.kill().await;
    }

    async fn record_error(&self, plugin_id: &str, error: &PluginRuntimeError) {
        self.last_errors
            .lock()
            .await
            .insert(plugin_id.to_owned(), error.to_string());
    }
}

#[derive(Clone, Debug, Default)]
pub struct PluginRuntimeStatus {
    pub running: bool,
    pub last_error: Option<String>,
}

struct PluginProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl PluginProcess {
    async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
        call_timeout: Duration,
    ) -> Result<serde_json::Value, PluginRuntimeError> {
        if self
            .child
            .try_wait()
            .map_err(PluginRuntimeError::Io)?
            .is_some()
        {
            return Err(PluginRuntimeError::Exited);
        }
        let request = PluginRequest::new(Uuid::now_v7().to_string(), method, params);
        let request_id = request.id.clone();
        let result = timeout(call_timeout, async {
            let mut line = serde_json::to_vec(&request).map_err(PluginRuntimeError::Json)?;
            line.push(b'\n');
            self.stdin
                .write_all(&line)
                .await
                .map_err(PluginRuntimeError::Io)?;
            self.stdin.flush().await.map_err(PluginRuntimeError::Io)?;
            let mut response_line = String::new();
            let bytes = self
                .stdout
                .read_line(&mut response_line)
                .await
                .map_err(PluginRuntimeError::Io)?;
            if bytes == 0 {
                return Err(PluginRuntimeError::Exited);
            }
            if response_line.len() > 4 * 1024 * 1024 {
                return Err(PluginRuntimeError::Protocol(
                    "plugin response is too large".to_owned(),
                ));
            }
            let response: PluginResponse =
                serde_json::from_str(&response_line).map_err(PluginRuntimeError::Json)?;
            if response.id != request_id {
                return Err(PluginRuntimeError::Protocol(
                    "plugin response id does not match request".to_owned(),
                ));
            }
            if let Some(error) = response.error {
                return Err(PluginRuntimeError::Plugin {
                    code: error.code,
                    message: error.message,
                });
            }
            response.result.ok_or_else(|| {
                PluginRuntimeError::Protocol("plugin response has no result".to_owned())
            })
        })
        .await
        .map_err(|_| PluginRuntimeError::Timeout)?;
        if result.is_err() {
            let _ = self.child.kill().await;
        }
        result
    }
}

fn spawn_process(
    plugin: &DiscoveredPlugin,
    config_dir: Option<&Path>,
    network_proxy_url: Option<&str>,
) -> Result<PluginProcess, PluginRuntimeError> {
    let entrypoint = absolute_runtime_path(&plugin.entrypoint).map_err(PluginRuntimeError::Io)?;
    let root_path = absolute_runtime_path(&plugin.root_path).map_err(PluginRuntimeError::Io)?;
    let config_dir = config_dir
        .map(absolute_runtime_path)
        .transpose()
        .map_err(PluginRuntimeError::Io)?;
    let mut command = Command::new(entrypoint);
    command
        .current_dir(root_path)
        .env("LUX_PLUGIN_ID", &plugin.manifest.id)
        .env(
            "LUX_PLUGIN_PROTOCOL_VERSION",
            plugin.manifest.api_version.to_string(),
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(config_dir) = config_dir {
        command.env("LUX_CONFIG_DIR", config_dir);
    }
    if let Some(proxy_url) = network_proxy_url {
        command.env("LUX_PROXY_URL", proxy_url);
    }
    let mut child = command.spawn().map_err(PluginRuntimeError::Io)?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| PluginRuntimeError::Protocol("plugin stdin was not captured".to_owned()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| PluginRuntimeError::Protocol("plugin stdout was not captured".to_owned()))?;
    if let Some(mut stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut sink = tokio::io::sink();
            let _ = tokio::io::copy(&mut stderr, &mut sink).await;
        });
    }
    Ok(PluginProcess {
        child,
        stdin: BufWriter::new(stdin),
        stdout: BufReader::new(stdout),
    })
}

fn absolute_runtime_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_owned());
    }
    std::env::current_dir().map(|current_dir| current_dir.join(path))
}

#[derive(Debug)]
pub enum PluginRuntimeError {
    UnknownPlugin(String),
    Io(io::Error),
    Json(serde_json::Error),
    Protocol(String),
    Plugin { code: String, message: String },
    Timeout,
    Exited,
}

impl fmt::Display for PluginRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPlugin(id) => write!(formatter, "unknown plugin: {id}"),
            Self::Io(error) => write!(formatter, "plugin process IO error: {error}"),
            Self::Json(error) => write!(formatter, "plugin protocol JSON error: {error}"),
            Self::Protocol(message) => write!(formatter, "plugin protocol error: {message}"),
            Self::Plugin { code, message } => write!(formatter, "plugin error {code}: {message}"),
            Self::Timeout => formatter.write_str("plugin request timed out"),
            Self::Exited => formatter.write_str("plugin process exited"),
        }
    }
}

impl std::error::Error for PluginRuntimeError {}

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
    let entrypoint_template = manifest
        .runtime
        .entrypoint
        .replace("${platform}", current_platform())
        .replace("${arch}", current_arch());
    let entrypoint = root.join(entrypoint_template);
    if !entrypoint.is_file() {
        return Err(PluginDiscoveryError::InvalidPackage(format!(
            "plugin entrypoint does not exist: {}",
            manifest.runtime.entrypoint
        )));
    }
    Ok(entrypoint)
}

fn current_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        platform => platform,
    }
}

fn current_arch() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "arm64",
        (_, arch) => arch,
    }
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
    if destination.exists() {
        fs::remove_dir_all(destination)?;
    }
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
