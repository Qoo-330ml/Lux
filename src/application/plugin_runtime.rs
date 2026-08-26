use std::{
    collections::{HashMap, HashSet},
    fmt,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{Mutex, RwLock, Semaphore, oneshot},
    time::{Instant, timeout_at},
};
use uuid::Uuid;
use zip::ZipArchive;

use super::plugin_protocol::{PluginManifest, PluginManifestError, PluginRequest, PluginResponse};

const MAX_PLUGIN_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_PLUGIN_UNCOMPRESSED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PLUGIN_FILES: usize = 512;
const MAX_PLUGIN_INFLIGHT: usize = 16;

#[derive(Clone, Debug)]
enum PluginConfigAccess {
    Shared(PathBuf),
    Dedicated(PathBuf),
    None,
}

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
        Self::discover_with_preference(plugin_dir, None)
    }

    pub fn discover_prefer(plugin_dir: &Path, plugin_id: &str, plugin_version: &str) -> Self {
        Self::discover_with_preference(plugin_dir, Some((plugin_id, plugin_version)))
    }

    fn discover_with_preference(plugin_dir: &Path, preferred_plugin: Option<(&str, &str)>) -> Self {
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
                Ok(plugin)
                    if preferred_plugin.is_some_and(|(id, version)| {
                        plugin.manifest.id == id && plugin.manifest.version == version
                    }) =>
                {
                    if let Some(index) = catalog
                        .plugins
                        .iter()
                        .position(|existing| existing.manifest.id == plugin.manifest.id)
                    {
                        let previous = std::mem::replace(&mut catalog.plugins[index], plugin);
                        catalog.failures.push(PluginDiscoveryFailure {
                            source_path: previous.source_path,
                            message: format!("duplicate plugin id: {}", previous.manifest.id),
                        });
                    } else {
                        catalog.plugins.push(plugin);
                    }
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

    pub fn get_by_alias(&self, alias: &str) -> Option<&DiscoveredPlugin> {
        self.plugins.iter().find(|plugin| {
            plugin
                .manifest
                .aliases
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(alias))
        })
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
    processes: Arc<Mutex<std::collections::HashMap<String, Arc<PluginProcess>>>>,
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
        self.call_with_config_access(plugin_id, method, params, true)
            .await
    }

    /// Calls a plugin without exposing Lux's shared configuration directory.
    ///
    /// Notification providers receive credentials in their RPC request. They
    /// must not inherit `LUX_CONFIG_DIR`, because that directory also contains
    /// unrelated plugin and server secrets.
    pub async fn call_without_config_access(
        &self,
        plugin_id: &str,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, PluginRuntimeError> {
        self.call_with_config_access(plugin_id, method, params, false)
            .await
    }

    async fn call_with_config_access(
        &self,
        plugin_id: &str,
        method: &str,
        params: serde_json::Value,
        allow_config_access: bool,
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
            if let Some(process) = processes.get(plugin_id).cloned()
                && !process.is_terminated()
            {
                process
            } else {
                processes.remove(plugin_id);
                let process = match spawn_process(
                    &plugin,
                    self.config_access(&plugin, allow_config_access),
                    self.network_proxy_url.as_deref(),
                ) {
                    Ok(process) => process,
                    Err(error) => {
                        self.record_error(plugin_id, &error).await;
                        return Err(error);
                    }
                };
                processes.insert(plugin_id.to_owned(), process.clone());
                process
            }
        };
        let result = process.call(method, params, self.call_timeout).await;
        if let Err(error) = &result {
            if is_process_failure(error) {
                let should_stop = {
                    let mut processes = self.processes.lock().await;
                    if processes
                        .get(plugin_id)
                        .is_some_and(|current| Arc::ptr_eq(current, &process))
                    {
                        processes.remove(plugin_id);
                        true
                    } else {
                        false
                    }
                };
                if should_stop {
                    process.stop().await;
                }
            }
            self.record_error(plugin_id, error).await;
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
        let process = match spawn_process(
            &plugin,
            self.config_access(&plugin, true),
            self.network_proxy_url.as_deref(),
        ) {
            Ok(process) => process,
            Err(error) => {
                self.record_error(plugin_id, &error).await;
                return Err(error);
            }
        };
        let result = process.call(method, params, self.call_timeout).await;
        process.stop().await;
        if let Err(error) = &result {
            self.record_error(plugin_id, error).await;
        } else {
            self.last_errors.lock().await.remove(plugin_id);
        }
        result
    }

    pub async fn status(&self, plugin_id: &str) -> PluginRuntimeStatus {
        let running = {
            let mut processes = self.processes.lock().await;
            match processes.get(plugin_id).cloned() {
                None => false,
                Some(process) if process.is_terminated() => {
                    processes.remove(plugin_id);
                    false
                }
                Some(_) => true,
            }
        };
        let last_error = self.last_errors.lock().await.get(plugin_id).cloned();
        PluginRuntimeStatus {
            running,
            last_error,
        }
    }

    pub async fn stop_all(&self) {
        let processes = std::mem::take(&mut *self.processes.lock().await);
        for process in processes.into_values() {
            process.stop().await;
        }
    }

    pub async fn stop(&self, plugin_id: &str) {
        let Some(process) = self.processes.lock().await.remove(plugin_id) else {
            return;
        };
        process.stop().await;
    }

    async fn record_error(&self, plugin_id: &str, error: &PluginRuntimeError) {
        self.last_errors
            .lock()
            .await
            .insert(plugin_id.to_owned(), error.to_string());
    }

    fn config_access(
        &self,
        plugin: &DiscoveredPlugin,
        allow_config_access: bool,
    ) -> PluginConfigAccess {
        if !allow_config_access {
            return PluginConfigAccess::None;
        }
        let Some(config_dir) = self.config_dir.as_ref() else {
            return PluginConfigAccess::None;
        };
        if plugin.manifest.plugin_type == "metadata" {
            PluginConfigAccess::Dedicated(
                config_dir
                    .join("plugin-config")
                    .join(format!("{}.json", plugin.manifest.id)),
            )
        } else {
            PluginConfigAccess::Shared(config_dir.clone())
        }
    }
}

fn is_process_failure(error: &PluginRuntimeError) -> bool {
    matches!(
        error,
        PluginRuntimeError::Io(_)
            | PluginRuntimeError::Json(_)
            | PluginRuntimeError::Protocol(_)
            | PluginRuntimeError::Timeout
            | PluginRuntimeError::Exited
    )
}

#[derive(Clone, Debug, Default)]
pub struct PluginRuntimeStatus {
    pub running: bool,
    pub last_error: Option<String>,
}

struct PluginProcess {
    child: Mutex<Child>,
    stdin: Mutex<BufWriter<ChildStdin>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Result<serde_json::Value, PluginRuntimeError>>>>,
    inflight: Semaphore,
    terminated: AtomicBool,
}

impl Drop for PluginProcess {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.try_lock() {
            let _ = child.start_kill();
        }
    }
}

impl PluginProcess {
    async fn call(
        self: &Arc<Self>,
        method: &str,
        params: serde_json::Value,
        call_timeout: Duration,
    ) -> Result<serde_json::Value, PluginRuntimeError> {
        let deadline = Instant::now() + call_timeout;
        let permit = match timeout_at(deadline, self.inflight.acquire()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return Err(PluginRuntimeError::Exited),
            Err(_) => {
                self.stop().await;
                return Err(PluginRuntimeError::Timeout);
            }
        };
        let child_status = timeout_at(deadline, async {
            let mut child = self.child.lock().await;
            child.try_wait().map_err(PluginRuntimeError::Io)
        })
        .await;
        match child_status {
            Ok(Ok(Some(_))) => {
                self.terminated.store(true, Ordering::Release);
                drop(permit);
                return Err(PluginRuntimeError::Exited);
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => {
                drop(permit);
                self.stop().await;
                return Err(error);
            }
            Err(_) => {
                drop(permit);
                self.stop().await;
                return Err(PluginRuntimeError::Timeout);
            }
        }
        let request = PluginRequest::new(Uuid::now_v7().to_string(), method, params);
        let request_id = request.id.clone();
        let (sender, receiver) = oneshot::channel();
        if timeout_at(deadline, async {
            self.pending.lock().await.insert(request_id.clone(), sender);
        })
        .await
        .is_err()
        {
            drop(permit);
            self.stop().await;
            return Err(PluginRuntimeError::Timeout);
        }

        let write_result = timeout_at(deadline, async {
            let mut line = serde_json::to_vec(&request).map_err(PluginRuntimeError::Json)?;
            line.push(b'\n');
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(&line)
                .await
                .map_err(PluginRuntimeError::Io)?;
            stdin.flush().await.map_err(PluginRuntimeError::Io)
        })
        .await;
        if let Err(error) = match write_result {
            Ok(result) => result,
            Err(_) => Err(PluginRuntimeError::Timeout),
        } {
            self.pending.lock().await.remove(&request_id);
            drop(permit);
            self.stop().await;
            return Err(error);
        }

        let result = match timeout_at(deadline, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(PluginRuntimeError::Exited),
            Err(_) => {
                self.pending.lock().await.remove(&request_id);
                self.stop().await;
                Err(PluginRuntimeError::Timeout)
            }
        };
        drop(permit);
        result
    }

    async fn read_responses(weak: Weak<Self>, mut stdout: BufReader<ChildStdout>) {
        loop {
            let mut response_line = String::new();
            let read_result = stdout.read_line(&mut response_line).await;
            let Some(process) = weak.upgrade() else {
                return;
            };
            let bytes = match read_result {
                Ok(bytes) => bytes,
                Err(error) => {
                    process
                        .fail_pending(format!("plugin stdout read failed: {error}"))
                        .await;
                    process.kill_child().await;
                    return;
                }
            };
            if bytes == 0 {
                process
                    .fail_pending("plugin process exited".to_owned())
                    .await;
                return;
            }
            if response_line.len() > 4 * 1024 * 1024 {
                process
                    .fail_pending("plugin response is too large".to_owned())
                    .await;
                process.kill_child().await;
                return;
            }
            let response: PluginResponse = match serde_json::from_str(&response_line) {
                Ok(response) => response,
                Err(error) => {
                    process
                        .fail_pending(format!("invalid plugin response: {error}"))
                        .await;
                    process.kill_child().await;
                    return;
                }
            };
            let Some(sender) = process.pending.lock().await.remove(&response.id) else {
                process
                    .fail_pending("plugin response ID has no pending request".to_owned())
                    .await;
                process.kill_child().await;
                return;
            };
            let _ = sender.send(response_result(response));
        }
    }

    async fn fail_pending(&self, message: String) {
        self.terminated.store(true, Ordering::Release);
        let pending = std::mem::take(&mut *self.pending.lock().await);
        for sender in pending.into_values() {
            let _ = sender.send(Err(PluginRuntimeError::Protocol(message.clone())));
        }
    }

    async fn kill_child(&self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }

    async fn stop(&self) {
        self.terminated.store(true, Ordering::Release);
        self.fail_pending("plugin process stopped".to_owned()).await;
        self.kill_child().await;
    }

    fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::Acquire)
    }
}

fn response_result(response: PluginResponse) -> Result<serde_json::Value, PluginRuntimeError> {
    if let Some(error) = response.error {
        return Err(PluginRuntimeError::Plugin {
            code: error.code,
            message: error.message,
        });
    }
    response
        .result
        .ok_or_else(|| PluginRuntimeError::Protocol("plugin response has no result".to_owned()))
}

fn spawn_process(
    plugin: &DiscoveredPlugin,
    config_access: PluginConfigAccess,
    network_proxy_url: Option<&str>,
) -> Result<Arc<PluginProcess>, PluginRuntimeError> {
    let entrypoint = absolute_runtime_path(&plugin.entrypoint).map_err(PluginRuntimeError::Io)?;
    let root_path = absolute_runtime_path(&plugin.root_path).map_err(PluginRuntimeError::Io)?;
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
    match config_access {
        PluginConfigAccess::Shared(config_dir) => {
            command.env_remove("LUX_PLUGIN_CONFIG_PATH").env(
                "LUX_CONFIG_DIR",
                absolute_runtime_path(&config_dir).map_err(PluginRuntimeError::Io)?,
            );
        }
        PluginConfigAccess::Dedicated(config_path) => {
            command.env_remove("LUX_CONFIG_DIR").env(
                "LUX_PLUGIN_CONFIG_PATH",
                absolute_runtime_path(&config_path).map_err(PluginRuntimeError::Io)?,
            );
        }
        PluginConfigAccess::None => {
            command
                .env_remove("LUX_CONFIG_DIR")
                .env_remove("LUX_PLUGIN_CONFIG_PATH");
        }
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
    let process = Arc::new(PluginProcess {
        child: Mutex::new(child),
        stdin: Mutex::new(BufWriter::new(stdin)),
        pending: Mutex::new(HashMap::new()),
        inflight: Semaphore::new(MAX_PLUGIN_INFLIGHT),
        terminated: AtomicBool::new(false),
    });
    let weak = Arc::downgrade(&process);
    tokio::spawn(PluginProcess::read_responses(weak, BufReader::new(stdout)));
    Ok(process)
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
