use std::{
    collections::HashMap,
    fmt,
    fmt::Write as _,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};

use tokio::{
    fs,
    io::AsyncReadExt,
    process::{Child, Command},
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
    time::sleep,
};

use super::decision::ServerTier;

const MAX_REMUX_SESSIONS: usize = 4;
const MAX_HARDWARE_SESSIONS: usize = 2;
const MAX_SOFTWARE_SESSIONS: usize = 1;
const MAX_SESSION_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DEFAULT_MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;
const MANIFEST_WAIT_ATTEMPTS: usize = 50;
const HLS_ASSET_WAIT_ATTEMPTS: usize = 600;
const HLS_SEGMENT_DURATION_TICKS: i64 = 4 * 10_000_000;
const HLS_SEGMENT_RESTART_GAP: i64 = 6;
const SESSION_QUOTA_CACHE_TTL: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub(crate) enum HlsError {
    Io(std::io::Error),
    Spawn(String),
    Limit,
    NotFound,
    Superseded,
    Failed,
    InvalidAsset,
}

impl fmt::Display for HlsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Spawn(message) => formatter.write_str(message),
            Self::Limit => formatter.write_str("HLS resource limit reached"),
            Self::NotFound => formatter.write_str("HLS session asset not found"),
            Self::Superseded => formatter.write_str("HLS asset request was superseded by a seek"),
            Self::Failed => {
                formatter.write_str("HLS process failed before producing the requested asset")
            }
            Self::InvalidAsset => formatter.write_str("invalid HLS asset"),
        }
    }
}

impl std::error::Error for HlsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Spawn(_)
            | Self::Limit
            | Self::NotFound
            | Self::Superseded
            | Self::Failed
            | Self::InvalidAsset => None,
        }
    }
}

impl HlsError {
    pub(crate) fn is_terminal_process_failure(&self) -> bool {
        matches!(
            self,
            Self::Io(_) | Self::Spawn(_) | Self::Limit | Self::Failed
        )
    }
}

struct HlsProcess {
    directory: PathBuf,
    initial_manifest: PathBuf,
    runtime_ticks: Option<i64>,
    specification: HlsProcessSpecification,
    state: Mutex<HlsProcessState>,
    restart: Mutex<()>,
    quota: Mutex<SessionQuotaCache>,
    _permit: OwnedSemaphorePermit,
}

struct HlsProcessSpecification {
    input: PathBuf,
    tier: ServerTier,
    video_bitrate: Option<i64>,
    emby_vod: bool,
}

struct HlsProcessState {
    child: Option<Child>,
    generation: u64,
    manifest: PathBuf,
    segment_start_number: i64,
}

struct SegmentSchedule {
    generation: u64,
    restarted: bool,
}

struct HlsOutput<'a> {
    manifest: &'a Path,
    preserve_input_timestamps: bool,
}

#[derive(Debug, Default)]
struct SessionQuotaCache {
    checked_at: Option<Instant>,
    total_bytes: u64,
}

impl SessionQuotaCache {
    fn is_fresh(&self, now: Instant) -> bool {
        self.checked_at.is_some_and(|checked_at| {
            now.saturating_duration_since(checked_at) < SESSION_QUOTA_CACHE_TTL
        })
    }
}

#[derive(Clone)]
pub(crate) struct HlsManager {
    base_directory: PathBuf,
    processes: Arc<Mutex<HashMap<String, Arc<HlsProcess>>>>,
    remux_slots: Arc<Semaphore>,
    hardware_slots: Arc<Semaphore>,
    software_slots: Arc<Semaphore>,
    hardware_encoder: Option<String>,
    ffmpeg_executable: String,
    min_free_bytes: u64,
}

impl HlsManager {
    pub(crate) fn new(config_dir: PathBuf) -> Self {
        Self::new_with_executable(config_dir, ffmpeg_executable())
    }

    fn new_with_executable(config_dir: PathBuf, ffmpeg_executable: String) -> Self {
        Self::new_with_limits(config_dir, ffmpeg_executable, DEFAULT_MIN_FREE_BYTES)
    }

    #[cfg(test)]
    pub(crate) fn new_for_tests(config_dir: PathBuf, ffmpeg_executable: String) -> Self {
        Self::new_with_limits(config_dir, ffmpeg_executable, 0)
    }

    fn new_with_limits(
        config_dir: PathBuf,
        ffmpeg_executable: String,
        min_free_bytes: u64,
    ) -> Self {
        let hardware_encoder = std::env::var("LUX_HLS_HW_ENCODER")
            .ok()
            .filter(|value| is_allowed_hardware_encoder(value));
        Self {
            base_directory: config_dir.join("web-playback"),
            processes: Arc::new(Mutex::new(HashMap::new())),
            remux_slots: Arc::new(Semaphore::new(MAX_REMUX_SESSIONS)),
            hardware_slots: Arc::new(Semaphore::new(MAX_HARDWARE_SESSIONS)),
            software_slots: Arc::new(Semaphore::new(MAX_SOFTWARE_SESSIONS)),
            hardware_encoder,
            ffmpeg_executable,
            min_free_bytes,
        }
    }

    pub(crate) fn hardware_transcode_available(&self) -> bool {
        self.hardware_encoder.is_some()
    }

    pub(crate) fn with_executable(mut self, executable: String) -> Self {
        self.ffmpeg_executable = executable;
        self
    }

    pub(crate) async fn start(
        &self,
        session_id: &str,
        tier: ServerTier,
        input: &Path,
        video_bitrate: Option<i64>,
        start_time_ticks: Option<i64>,
        runtime_ticks: Option<i64>,
    ) -> Result<(), HlsError> {
        self.start_process(
            session_id,
            HlsProcessSpecification {
                input: input.to_path_buf(),
                tier,
                video_bitrate,
                emby_vod: false,
            },
            start_time_ticks,
            runtime_ticks,
        )
        .await
    }

    pub(crate) async fn start_emby_vod(
        &self,
        session_id: &str,
        tier: ServerTier,
        input: &Path,
        video_bitrate: Option<i64>,
        runtime_ticks: Option<i64>,
    ) -> Result<(), HlsError> {
        self.start_process(
            session_id,
            HlsProcessSpecification {
                input: input.to_path_buf(),
                tier,
                video_bitrate,
                emby_vod: true,
            },
            None,
            runtime_ticks,
        )
        .await
    }

    async fn start_process(
        &self,
        session_id: &str,
        specification: HlsProcessSpecification,
        start_time_ticks: Option<i64>,
        runtime_ticks: Option<i64>,
    ) -> Result<(), HlsError> {
        let tier = specification.tier;
        let permit = self.acquire_permit(tier).await?;
        fs::create_dir_all(&self.base_directory)
            .await
            .map_err(HlsError::Io)?;
        if !has_sufficient_free_space(&self.base_directory, self.min_free_bytes) {
            return Err(HlsError::Limit);
        }
        let directory = self.base_directory.join(session_id);
        fs::create_dir_all(&directory).await.map_err(HlsError::Io)?;
        let segment_start_number = hls_start_number(start_time_ticks);
        let generation = 0;
        let manifest = hls_manifest_path(&directory, specification.emby_vod, generation);
        let child = match self.spawn_child(&specification, &directory, &manifest, start_time_ticks)
        {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_dir_all(&directory).await;
                return Err(error);
            }
        };
        let process = Arc::new(HlsProcess {
            directory,
            initial_manifest: manifest.clone(),
            runtime_ticks: runtime_ticks.filter(|ticks| *ticks > 0),
            specification,
            state: Mutex::new(HlsProcessState {
                child: Some(child),
                generation,
                manifest,
                segment_start_number,
            }),
            restart: Mutex::new(()),
            quota: Mutex::new(SessionQuotaCache::default()),
            _permit: permit,
        });
        let previous = self
            .processes
            .lock()
            .await
            .insert(session_id.to_owned(), process);
        if let Some(previous) = previous {
            stop_process(previous).await;
        }
        Ok(())
    }

    fn spawn_child(
        &self,
        specification: &HlsProcessSpecification,
        directory: &Path,
        manifest: &Path,
        start_time_ticks: Option<i64>,
    ) -> Result<Child, HlsError> {
        let args = if specification.emby_vod {
            ffmpeg_args_for_output(
                &specification.input,
                directory,
                specification.tier,
                self.hardware_encoder.as_deref(),
                specification.video_bitrate,
                start_time_ticks,
                HlsOutput {
                    manifest,
                    preserve_input_timestamps: true,
                },
            )?
        } else {
            ffmpeg_args(
                &specification.input,
                directory,
                specification.tier,
                self.hardware_encoder.as_deref(),
                specification.video_bitrate,
                start_time_ticks,
            )?
        };
        let mut command = Command::new(&self.ffmpeg_executable);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .map_err(|error| HlsError::Spawn(format!("failed to start HLS process: {error}")))?;
        if let Some(mut stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut buffer = [0_u8; 4096];
                loop {
                    match stderr.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            });
        }
        Ok(child)
    }

    pub(crate) async fn wait_for_manifest(&self, session_id: &str) -> Result<PathBuf, HlsError> {
        let process = self
            .processes
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        let manifest = process.initial_manifest.clone();
        for _ in 0..MANIFEST_WAIT_ATTEMPTS {
            if fs::metadata(&manifest).await.is_ok() {
                return Ok(manifest);
            }
            let finished = {
                let mut state = process.state.lock().await;
                match state.child.as_mut() {
                    Some(child) => child.try_wait().map_err(HlsError::Io)?.is_some(),
                    None => true,
                }
            };
            if finished {
                return Err(HlsError::Failed);
            }
            sleep(Duration::from_millis(100)).await;
        }
        Err(HlsError::Failed)
    }

    pub(crate) async fn asset_path(
        &self,
        session_id: &str,
        asset: &str,
    ) -> Result<PathBuf, HlsError> {
        if !is_valid_asset(asset) {
            return Err(HlsError::InvalidAsset);
        }
        let process = self
            .processes
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        asset_segment_number(asset)?;
        let path = process.directory.join(asset);
        if !path.starts_with(&process.directory) {
            return Err(HlsError::InvalidAsset);
        }
        Ok(path)
    }

    pub(crate) async fn wait_for_asset(
        &self,
        session_id: &str,
        asset: &str,
    ) -> Result<PathBuf, HlsError> {
        if !is_valid_asset(asset) {
            return Err(HlsError::InvalidAsset);
        }
        let segment_number = asset_segment_number(asset)?;
        let process = self
            .processes
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        let path = process.directory.join(asset);
        if !path.starts_with(&process.directory) {
            return Err(HlsError::InvalidAsset);
        }
        let mut schedule = if let Some(segment_number) = segment_number
            && process.specification.emby_vod
        {
            Some(
                self.restart_for_segment(session_id, &process, segment_number, &path)
                    .await?,
            )
        } else {
            None
        };
        for _ in 0..HLS_ASSET_WAIT_ATTEMPTS {
            if fs::metadata(&path)
                .await
                .is_ok_and(|metadata| metadata.is_file())
            {
                return Ok(path);
            }
            let current_process = self
                .processes
                .lock()
                .await
                .get(session_id)
                .cloned()
                .ok_or(HlsError::NotFound)?;
            if !Arc::ptr_eq(&current_process, &process) {
                return Err(HlsError::NotFound);
            }
            let (generation, finished) = {
                let mut state = process.state.lock().await;
                let finished = match state.child.as_mut() {
                    Some(child) => child.try_wait().map_err(HlsError::Io)?.is_some(),
                    None => true,
                };
                (state.generation, finished)
            };
            if let Some(segment_number) = segment_number
                && process.specification.emby_vod
                && schedule
                    .as_ref()
                    .is_some_and(|scheduled| scheduled.generation != generation)
            {
                if !segment_is_near_current_generation(&process, segment_number).await? {
                    return Err(HlsError::Superseded);
                }
                schedule = Some(SegmentSchedule {
                    generation,
                    restarted: false,
                });
            }
            if finished {
                if let Some(segment_number) = segment_number
                    && process.specification.emby_vod
                    && schedule
                        .as_ref()
                        .is_some_and(|scheduled| !scheduled.restarted)
                {
                    schedule = Some(
                        self.restart_for_segment(session_id, &process, segment_number, &path)
                            .await?,
                    );
                    continue;
                }
                return Err(HlsError::Failed);
            }
            sleep(Duration::from_millis(100)).await;
        }
        Err(HlsError::Failed)
    }

    async fn restart_for_segment(
        &self,
        session_id: &str,
        process: &Arc<HlsProcess>,
        requested_segment: i64,
        requested_path: &Path,
    ) -> Result<SegmentSchedule, HlsError> {
        let _restart = process.restart.lock().await;
        let is_current_process = self
            .processes
            .lock()
            .await
            .get(session_id)
            .is_some_and(|current| Arc::ptr_eq(current, process));
        if !is_current_process {
            return Err(HlsError::NotFound);
        }
        if fs::metadata(requested_path)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            let generation = process.state.lock().await.generation;
            return Ok(SegmentSchedule {
                generation,
                restarted: false,
            });
        }
        let mut state = process.state.lock().await;
        let latest_segment = latest_manifest_segment_number(&state.manifest).await?;
        let finished = match state.child.as_mut() {
            Some(child) => child.try_wait().map_err(HlsError::Io)?.is_some(),
            None => true,
        };
        let current_segment =
            latest_segment.unwrap_or_else(|| state.segment_start_number.saturating_sub(1));
        let restart_reason = if finished {
            Some("process_finished")
        } else if requested_segment < state.segment_start_number {
            Some("backward_seek")
        } else if requested_segment.saturating_sub(current_segment) > HLS_SEGMENT_RESTART_GAP {
            Some("forward_seek")
        } else {
            None
        };
        let Some(restart_reason) = restart_reason else {
            return Ok(SegmentSchedule {
                generation: state.generation,
                restarted: false,
            });
        };

        self.check_restart_resources(process).await?;
        stop_child(&mut state.child).await;
        let start_time_ticks = requested_segment
            .checked_mul(HLS_SEGMENT_DURATION_TICKS)
            .ok_or(HlsError::InvalidAsset)?;
        let generation = state.generation.saturating_add(1);
        let manifest = hls_manifest_path(&process.directory, true, generation);
        let child = self.spawn_child(
            &process.specification,
            &process.directory,
            &manifest,
            Some(start_time_ticks),
        )?;
        tracing::info!(
            event = "emby_hls_segment_restart",
            session_id_prefix = %session_id.chars().take(8).collect::<String>(),
            segment_number = requested_segment,
            current_segment_number = current_segment,
            reason = restart_reason,
            "restarted Emby HLS transcoding at requested segment"
        );
        state.child = Some(child);
        state.generation = generation;
        state.manifest = manifest;
        state.segment_start_number = requested_segment;
        Ok(SegmentSchedule {
            generation,
            restarted: true,
        })
    }

    async fn check_restart_resources(&self, process: &HlsProcess) -> Result<(), HlsError> {
        if !has_sufficient_free_space(&self.base_directory, self.min_free_bytes) {
            return Err(HlsError::Limit);
        }
        let total_bytes = session_directory_bytes(&process.directory).await?;
        let mut quota = process.quota.lock().await;
        quota.total_bytes = total_bytes;
        quota.checked_at = Some(Instant::now());
        if total_bytes > MAX_SESSION_BYTES {
            return Err(HlsError::Limit);
        }
        Ok(())
    }

    pub(crate) async fn vod_manifest(&self, session_id: &str) -> Result<Option<String>, HlsError> {
        let process = self
            .processes
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        let Some(runtime_ticks) = process.runtime_ticks else {
            return Ok(None);
        };
        Ok(vod_manifest(runtime_ticks))
    }

    pub(crate) async fn session_directory(&self, session_id: &str) -> Result<PathBuf, HlsError> {
        let process = self
            .processes
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        Ok(process.directory.clone())
    }

    pub(crate) async fn within_quota(&self, session_id: &str) -> Result<bool, HlsError> {
        let process = self
            .processes
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        let mut quota = process.quota.lock().await;
        let now = Instant::now();
        if quota.is_fresh(now) {
            return Ok(quota.total_bytes <= MAX_SESSION_BYTES);
        }
        let total = match session_directory_bytes(&process.directory).await {
            Ok(total) => total,
            Err(error) => {
                quota.checked_at = None;
                return Err(error);
            }
        };
        quota.total_bytes = total;
        quota.checked_at = Some(Instant::now());
        Ok(total <= MAX_SESSION_BYTES)
    }

    pub(crate) async fn stop(&self, session_id: &str) -> Result<(), HlsError> {
        let process = self.processes.lock().await.remove(session_id);
        if let Some(process) = process {
            stop_process(process).await;
        } else {
            let directory = self.base_directory.join(session_id);
            if fs::metadata(&directory).await.is_ok() {
                fs::remove_dir_all(directory).await.map_err(HlsError::Io)?;
            }
        }
        Ok(())
    }

    pub(crate) async fn cleanup_orphans(&self) -> Result<(), HlsError> {
        fs::create_dir_all(&self.base_directory)
            .await
            .map_err(HlsError::Io)?;
        let mut entries = fs::read_dir(&self.base_directory)
            .await
            .map_err(HlsError::Io)?;
        while let Some(entry) = entries.next_entry().await.map_err(HlsError::Io)? {
            if entry.file_type().await.map_err(HlsError::Io)?.is_dir() {
                fs::remove_dir_all(entry.path())
                    .await
                    .map_err(HlsError::Io)?;
            }
        }
        Ok(())
    }

    async fn acquire_permit(&self, tier: ServerTier) -> Result<OwnedSemaphorePermit, HlsError> {
        let semaphore = match tier {
            ServerTier::Remux | ServerTier::AudioTranscode => &self.remux_slots,
            ServerTier::HardwareTranscode => {
                if self.hardware_encoder.is_none() {
                    return Err(HlsError::Limit);
                }
                &self.hardware_slots
            }
            ServerTier::SoftwareTranscode => &self.software_slots,
            ServerTier::Direct => return Err(HlsError::InvalidAsset),
        };
        semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| HlsError::Limit)
    }
}

async fn session_directory_bytes(directory: &Path) -> Result<u64, HlsError> {
    let mut entries = fs::read_dir(directory).await.map_err(HlsError::Io)?;
    let mut total = 0_u64;
    while let Some(entry) = entries.next_entry().await.map_err(HlsError::Io)? {
        let metadata = entry.metadata().await.map_err(HlsError::Io)?;
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
            if total > MAX_SESSION_BYTES {
                return Ok(total);
            }
        }
    }
    Ok(total)
}

async fn latest_manifest_segment_number(manifest_path: &Path) -> Result<Option<i64>, HlsError> {
    let manifest = match fs::read_to_string(manifest_path).await {
        Ok(manifest) => manifest,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(HlsError::Io(error)),
    };
    Ok(manifest
        .lines()
        .filter_map(|line| asset_segment_number(line.trim()).ok().flatten())
        .max())
}

async fn segment_is_near_current_generation(
    process: &HlsProcess,
    requested_segment: i64,
) -> Result<bool, HlsError> {
    let (manifest, segment_start_number) = {
        let state = process.state.lock().await;
        (state.manifest.clone(), state.segment_start_number)
    };
    let latest_segment = latest_manifest_segment_number(&manifest).await?;
    let current_segment = latest_segment.unwrap_or_else(|| segment_start_number.saturating_sub(1));
    Ok(requested_segment >= segment_start_number
        && requested_segment.saturating_sub(current_segment) <= HLS_SEGMENT_RESTART_GAP)
}

fn hls_manifest_path(directory: &Path, emby_vod: bool, generation: u64) -> PathBuf {
    if emby_vod {
        directory.join(format!("generation_{generation:06}.m3u8"))
    } else {
        directory.join("index.m3u8")
    }
}

async fn stop_process(process: Arc<HlsProcess>) {
    let _restart = process.restart.lock().await;
    let mut state = process.state.lock().await;
    stop_child(&mut state.child).await;
    drop(state);
    let _ = fs::remove_dir_all(&process.directory).await;
}

async fn stop_child(child: &mut Option<Child>) {
    if let Some(mut child) = child.take() {
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            kill_process_group(pid);
        }
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // FFmpeg is started in its own process group so helper processes cannot
    // outlive a stopped Web playback session.
    let _ = unsafe { libc::kill(-pid, libc::SIGKILL) };
}

fn ffmpeg_executable() -> String {
    std::env::var("LUX_FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_owned())
}

fn ffmpeg_args(
    input: &Path,
    directory: &Path,
    tier: ServerTier,
    hardware_encoder: Option<&str>,
    video_bitrate: Option<i64>,
    start_time_ticks: Option<i64>,
) -> Result<Vec<String>, HlsError> {
    ffmpeg_args_with_timeline(
        input,
        directory,
        tier,
        hardware_encoder,
        video_bitrate,
        start_time_ticks,
        false,
    )
}

fn ffmpeg_args_with_timeline(
    input: &Path,
    directory: &Path,
    tier: ServerTier,
    hardware_encoder: Option<&str>,
    video_bitrate: Option<i64>,
    start_time_ticks: Option<i64>,
    preserve_input_timestamps: bool,
) -> Result<Vec<String>, HlsError> {
    let manifest = directory.join("index.m3u8");
    ffmpeg_args_for_output(
        input,
        directory,
        tier,
        hardware_encoder,
        video_bitrate,
        start_time_ticks,
        HlsOutput {
            manifest: &manifest,
            preserve_input_timestamps,
        },
    )
}

fn ffmpeg_args_for_output(
    input: &Path,
    directory: &Path,
    tier: ServerTier,
    hardware_encoder: Option<&str>,
    video_bitrate: Option<i64>,
    start_time_ticks: Option<i64>,
    output: HlsOutput<'_>,
) -> Result<Vec<String>, HlsError> {
    if tier == ServerTier::Direct {
        return Err(HlsError::InvalidAsset);
    }
    if tier == ServerTier::HardwareTranscode && hardware_encoder.is_none() {
        return Err(HlsError::Limit);
    }
    let mut args = vec![
        "-hide_banner".to_owned(),
        "-loglevel".to_owned(),
        "warning".to_owned(),
        "-nostdin".to_owned(),
    ];
    let start_time = ffmpeg_start_time(start_time_ticks);
    if let Some(start_time) = start_time.as_ref() {
        args.extend(["-ss".to_owned(), start_time.clone()]);
    }
    args.extend([
        "-i".to_owned(),
        input.to_string_lossy().into_owned(),
        "-map".to_owned(),
        "0:v:0?".to_owned(),
        "-map".to_owned(),
        "0:a:0?".to_owned(),
    ]);
    match tier {
        ServerTier::Remux => {
            args.extend([
                "-c:v".to_owned(),
                "copy".to_owned(),
                "-c:a".to_owned(),
                "copy".to_owned(),
            ]);
        }
        ServerTier::AudioTranscode => {
            args.extend([
                "-c:v".to_owned(),
                "copy".to_owned(),
                "-c:a".to_owned(),
                "aac".to_owned(),
                "-b:a".to_owned(),
                "192k".to_owned(),
            ]);
        }
        ServerTier::HardwareTranscode => {
            args.extend([
                "-c:v".to_owned(),
                hardware_encoder.unwrap_or_default().to_owned(),
                "-c:a".to_owned(),
                "aac".to_owned(),
                "-b:a".to_owned(),
                "192k".to_owned(),
            ]);
        }
        ServerTier::SoftwareTranscode => {
            args.extend([
                "-c:v".to_owned(),
                "libx264".to_owned(),
                "-preset".to_owned(),
                "veryfast".to_owned(),
                "-pix_fmt".to_owned(),
                "yuv420p".to_owned(),
                "-c:a".to_owned(),
                "aac".to_owned(),
                "-b:a".to_owned(),
                "192k".to_owned(),
            ]);
        }
        ServerTier::Direct => unreachable!(),
    }
    if matches!(
        tier,
        ServerTier::HardwareTranscode | ServerTier::SoftwareTranscode
    ) && let Some(video_bitrate) = video_bitrate.filter(|value| *value > 0)
    {
        args.extend(["-b:v".to_owned(), video_bitrate.to_string()]);
    }
    if output.preserve_input_timestamps
        && matches!(
            tier,
            ServerTier::HardwareTranscode | ServerTier::SoftwareTranscode
        )
    {
        // Emby's VOD manifest exposes a complete four-second timeline before
        // FFmpeg has produced all assets. Keep encoded video keyframes on the
        // same cadence so the virtual segment numbers remain real files.
        args.extend([
            "-force_key_frames".to_owned(),
            "expr:gte(t,n_forced*4)".to_owned(),
            "-sc_threshold".to_owned(),
            "0".to_owned(),
        ]);
    }
    if output.preserve_input_timestamps {
        args.extend([
            "-copyts".to_owned(),
            "-avoid_negative_ts".to_owned(),
            "disabled".to_owned(),
        ]);
    } else if let Some(start_time) = start_time {
        // Input seeking resets encoded timestamps to zero. Emby's complete VOD
        // timeline uses `-copyts`; keep the existing output shift only for Lux
        // Web's dynamic HLS seek behavior.
        args.extend(["-output_ts_offset".to_owned(), start_time]);
    }
    args.extend([
        "-f".to_owned(),
        "hls".to_owned(),
        "-hls_time".to_owned(),
        "4".to_owned(),
        "-hls_list_size".to_owned(),
        "0".to_owned(),
        "-hls_segment_type".to_owned(),
        "fmp4".to_owned(),
        "-start_number".to_owned(),
        hls_start_number(start_time_ticks).to_string(),
        "-hls_fmp4_init_filename".to_owned(),
        "init.mp4".to_owned(),
        "-hls_segment_filename".to_owned(),
        directory
            .join("segment_%06d.m4s")
            .to_string_lossy()
            .into_owned(),
        "-hls_flags".to_owned(),
        "independent_segments+temp_file".to_owned(),
    ]);
    if output.preserve_input_timestamps {
        args.push("-y".to_owned());
    }
    args.push(output.manifest.to_string_lossy().into_owned());
    Ok(args)
}

fn hls_start_number(start_time_ticks: Option<i64>) -> i64 {
    start_time_ticks
        .filter(|ticks| *ticks > 0)
        .map_or(0, |ticks| ticks / HLS_SEGMENT_DURATION_TICKS)
}

fn asset_segment_number(asset: &str) -> Result<Option<i64>, HlsError> {
    let Some(segment_number) = asset
        .strip_prefix("segment_")
        .and_then(|value| value.strip_suffix(".m4s"))
    else {
        return Ok(None);
    };
    let number = segment_number
        .parse::<i64>()
        .map_err(|_| HlsError::InvalidAsset)?;
    if number < 0 {
        return Err(HlsError::InvalidAsset);
    }
    Ok(Some(number))
}

fn ffmpeg_start_time(start_time_ticks: Option<i64>) -> Option<String> {
    const TICKS_PER_SECOND: i64 = 10_000_000;
    let ticks = start_time_ticks.filter(|ticks| *ticks > 0)?;
    let seconds = ticks / TICKS_PER_SECOND;
    let remainder = ticks % TICKS_PER_SECOND;
    if remainder == 0 {
        Some(seconds.to_string())
    } else {
        Some(format!("{seconds}.{remainder:07}"))
    }
}

fn vod_manifest(runtime_ticks: i64) -> Option<String> {
    if runtime_ticks <= 0 {
        return None;
    }
    const TICKS_PER_SECOND: i64 = 10_000_000;
    let full_segments = runtime_ticks / HLS_SEGMENT_DURATION_TICKS;
    let remainder_ticks = runtime_ticks % HLS_SEGMENT_DURATION_TICKS;
    let segment_count = full_segments + i64::from(remainder_ticks > 0);
    let target_duration = if full_segments > 0 {
        HLS_SEGMENT_DURATION_TICKS / TICKS_PER_SECOND
    } else {
        (runtime_ticks + TICKS_PER_SECOND - 1) / TICKS_PER_SECOND
    };
    let mut manifest = String::new();
    let _ = writeln!(manifest, "#EXTM3U");
    let _ = writeln!(manifest, "#EXT-X-PLAYLIST-TYPE:VOD");
    let _ = writeln!(manifest, "#EXT-X-VERSION:7");
    let _ = writeln!(manifest, "#EXT-X-TARGETDURATION:{target_duration}");
    let _ = writeln!(manifest, "#EXT-X-MEDIA-SEQUENCE:0");
    let _ = writeln!(manifest, "#EXT-X-MAP:URI=\"init.mp4\"");
    for index in 0..segment_count {
        let duration_ticks = if index < full_segments {
            HLS_SEGMENT_DURATION_TICKS
        } else {
            remainder_ticks
        };
        let duration = hls_duration_seconds(duration_ticks);
        let _ = writeln!(manifest, "#EXTINF:{duration},");
        let _ = writeln!(manifest, "segment_{index:06}.m4s");
    }
    let _ = writeln!(manifest, "#EXT-X-ENDLIST");
    Some(manifest)
}

fn hls_duration_seconds(ticks: i64) -> String {
    const TICKS_PER_SECOND: i64 = 10_000_000;
    let seconds = ticks / TICKS_PER_SECOND;
    let micros = (ticks % TICKS_PER_SECOND) * 1_000_000 / TICKS_PER_SECOND;
    format!("{seconds}.{micros:06}")
}

fn is_valid_asset(asset: &str) -> bool {
    if asset.is_empty() || asset.len() > 128 {
        return false;
    }
    let path = Path::new(asset);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return false;
    }
    asset == "index.m3u8"
        || asset == "init.mp4"
        || (asset.starts_with("segment_") && asset.ends_with(".m4s"))
}

fn is_allowed_hardware_encoder(value: &str) -> bool {
    matches!(
        value,
        "h264_nvenc" | "h264_vaapi" | "h264_qsv" | "h264_videotoolbox"
    )
}

fn has_sufficient_free_space(path: &Path, minimum: u64) -> bool {
    available_free_bytes(path).is_ok_and(|available| available >= minimum)
}

#[cfg(unix)]
fn available_free_bytes(path: &Path) -> Result<u64, std::io::Error> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("free-space path contains a NUL byte"))?;
    let mut statistics = MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `statistics` points to writable memory for libc to initialize and
    // the C string is NUL-terminated for the duration of the call.
    let result = unsafe { libc::statvfs(path.as_ptr(), statistics.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: statvfs returned success, so libc initialized `statistics`.
    let statistics = unsafe { statistics.assume_init() };
    (statistics.f_bavail as u64)
        .checked_mul(statistics.f_frsize)
        .ok_or_else(|| std::io::Error::other("free-space value overflowed"))
}

#[cfg(not(unix))]
fn available_free_bytes(_path: &Path) -> Result<u64, std::io::Error> {
    Ok(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        time::{Duration, Instant},
    };

    use super::{
        HLS_SEGMENT_DURATION_TICKS, SESSION_QUOTA_CACHE_TTL, ServerTier, SessionQuotaCache,
        asset_segment_number, ffmpeg_args, ffmpeg_args_with_timeline, hls_start_number,
        is_valid_asset, vod_manifest,
    };

    #[test]
    fn vod_manifest_declares_the_complete_runtime() {
        let manifest = vod_manifest(9 * 10_000_000 + 5_000_000).expect("positive runtime");

        assert!(manifest.contains("#EXT-X-PLAYLIST-TYPE:VOD\n"));
        assert!(manifest.contains("#EXT-X-ENDLIST\n"));
        assert_eq!(manifest.matches("#EXTINF:").count(), 3);
        assert_eq!(manifest.matches("segment_").count(), 3);
        assert!(manifest.contains("#EXTINF:4.000000,"));
        assert!(manifest.contains("#EXTINF:1.500000,"));
        assert!(manifest.contains("#EXT-X-TARGETDURATION:4\n"));
        assert_eq!(HLS_SEGMENT_DURATION_TICKS, 4 * 10_000_000);
    }

    #[test]
    fn remux_arguments_copy_video_and_audio_into_cmaf_hls() {
        let args = ffmpeg_args(
            Path::new("/media/movie.mkv"),
            Path::new("/config/web-playback/session"),
            ServerTier::Remux,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "copy"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:a", "copy"]));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-hls_segment_type", "fmp4"])
        );
        assert!(
            !args
                .windows(2)
                .any(|pair| pair == ["-hls_playlist_type", "vod"])
        );
        assert!(
            args.iter()
                .any(|value| value == "segment_%06d.m4s" || value.ends_with("segment_%06d.m4s"))
        );
    }

    #[test]
    fn software_arguments_encode_with_aac_and_x264() {
        let args = ffmpeg_args(
            Path::new("movie.mkv"),
            Path::new("session"),
            ServerTier::SoftwareTranscode,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "libx264"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:a", "aac"]));
    }

    #[test]
    fn video_transcoding_arguments_apply_requested_video_bitrate() {
        let args = ffmpeg_args(
            Path::new("movie.mkv"),
            Path::new("session"),
            ServerTier::SoftwareTranscode,
            None,
            Some(1_000_000),
            None,
        )
        .unwrap();

        assert!(args.windows(2).any(|pair| pair == ["-b:v", "1000000"]));
    }

    #[test]
    fn emby_video_transcoding_arguments_align_with_the_four_second_vod_timeline() {
        let args = ffmpeg_args_with_timeline(
            Path::new("movie.mkv"),
            Path::new("session"),
            ServerTier::SoftwareTranscode,
            None,
            None,
            None,
            true,
        )
        .unwrap();

        assert!(
            args.windows(2)
                .any(|pair| pair == ["-force_key_frames", "expr:gte(t,n_forced*4)"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-sc_threshold", "0"]));
    }

    #[test]
    fn web_video_transcoding_does_not_force_emby_vod_keyframes() {
        let args = ffmpeg_args(
            Path::new("movie.mkv"),
            Path::new("session"),
            ServerTier::SoftwareTranscode,
            None,
            None,
            None,
        )
        .unwrap();

        assert!(!args.iter().any(|value| value == "-force_key_frames"));
        assert!(!args.iter().any(|value| value == "-sc_threshold"));
    }

    #[test]
    fn resume_arguments_seek_before_input_and_offset_output_to_original_timeline() {
        let args = ffmpeg_args(
            Path::new("movie.mkv"),
            Path::new("session"),
            ServerTier::SoftwareTranscode,
            None,
            None,
            Some(12_345_678),
        )
        .unwrap();

        let seek = args
            .windows(2)
            .position(|pair| pair == ["-ss", "1.2345678"])
            .expect("resume seek arguments");
        let input = args
            .iter()
            .position(|value| value == "-i")
            .expect("input argument");
        let output_offset = args
            .windows(2)
            .position(|pair| pair == ["-output_ts_offset", "1.2345678"])
            .expect("output timestamp offset arguments");
        assert!(seek < input);
        assert!(input < output_offset);
    }

    #[test]
    fn playback_from_start_does_not_offset_output_timestamps() {
        let args = ffmpeg_args(
            Path::new("movie.mkv"),
            Path::new("session"),
            ServerTier::SoftwareTranscode,
            None,
            None,
            None,
        )
        .unwrap();

        assert!(!args.iter().any(|value| value == "-output_ts_offset"));
    }

    #[test]
    fn resumed_output_uses_the_virtual_segment_number_for_the_start_time() {
        let args = ffmpeg_args(
            Path::new("movie.mkv"),
            Path::new("session"),
            ServerTier::SoftwareTranscode,
            None,
            None,
            Some(8 * 10_000_000),
        )
        .unwrap();

        assert!(args.windows(2).any(|pair| pair == ["-start_number", "2"]));
    }

    #[test]
    fn hls_start_number_uses_the_four_second_timeline() {
        assert_eq!(hls_start_number(None), 0);
        assert_eq!(hls_start_number(Some(4 * 10_000_000)), 1);
        assert_eq!(hls_start_number(Some(9 * 10_000_000)), 2);
    }

    #[test]
    fn segment_assets_keep_their_logical_number() {
        assert_eq!(asset_segment_number("segment_000000.m4s").unwrap(), Some(0));
        assert_eq!(
            asset_segment_number("segment_000969.m4s").unwrap(),
            Some(969)
        );
        assert_eq!(asset_segment_number("init.mp4").unwrap(), None);
        assert!(asset_segment_number("segment_invalid.m4s").is_err());
    }

    #[test]
    fn emby_restart_preserves_input_timestamps_on_the_vod_timeline() {
        let args = ffmpeg_args_with_timeline(
            Path::new("movie.mkv"),
            Path::new("session"),
            ServerTier::SoftwareTranscode,
            None,
            None,
            Some(8 * 10_000_000),
            true,
        )
        .unwrap();

        assert!(args.windows(2).any(|pair| pair == ["-ss", "8"]));
        assert!(args.iter().any(|value| value == "-copyts"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-avoid_negative_ts", "disabled"])
        );
        assert!(args.windows(2).any(|pair| pair == ["-start_number", "2"]));
        assert!(!args.iter().any(|value| value == "-output_ts_offset"));
    }

    #[test]
    fn asset_validation_rejects_path_traversal_and_unknown_files() {
        assert!(is_valid_asset("index.m3u8"));
        assert!(is_valid_asset("segment_000001.m4s"));
        assert!(!is_valid_asset("../index.m3u8"));
        assert!(!is_valid_asset("other.txt"));
    }

    #[test]
    fn session_quota_cache_expires_quickly() {
        let checked_at = Instant::now();
        let cache = SessionQuotaCache {
            checked_at: Some(checked_at),
            total_bytes: 1,
        };

        assert!(cache.is_fresh(checked_at + Duration::from_millis(100)));
        assert!(!cache.is_fresh(checked_at + SESSION_QUOTA_CACHE_TTL));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn manager_drains_a_process_and_cleans_its_session_directory() {
        use std::{os::unix::fs::PermissionsExt, path::PathBuf};

        let temp_dir = tempfile::tempdir().unwrap();
        let script = temp_dir.path().join("fake-ffmpeg");
        let script_body = "#!/bin/sh\nset -eu\nmanifest=\"\"\nsegment=\"\"\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -hls_segment_filename) segment=\"$2\"; shift 2 ;;\n    *.m3u8) manifest=\"$1\"; shift ;;\n    *) shift ;;\n  esac\ndone\ndirectory=$(dirname \"$manifest\")\nmkdir -p \"$directory\"\nprintf '#EXTM3U\\n#EXT-X-MAP:URI=\\\"init.mp4\\\"\\n#EXTINF:1,\\nsegment_000000.m4s\\n' > \"$manifest\"\nprintf init > \"$directory/init.mp4\"\nprintf segment > \"$(printf '%s' \"$segment\" | sed 's/%06d/000000/')\"\n";
        tokio::fs::write(&script, script_body).await.unwrap();
        let mut permissions = tokio::fs::metadata(&script).await.unwrap().permissions();
        permissions.set_mode(0o700);
        tokio::fs::set_permissions(&script, permissions)
            .await
            .unwrap();
        let manager = super::HlsManager::new_with_executable(
            temp_dir.path().join("config"),
            script.to_string_lossy().into_owned(),
        );
        manager
            .start(
                "session-1",
                ServerTier::Remux,
                Path::new("input.mkv"),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        let manifest = manager.wait_for_manifest("session-1").await.unwrap();
        assert!(
            tokio::fs::read_to_string(manifest)
                .await
                .unwrap()
                .contains("segment_000000.m4s")
        );
        let init = manager.asset_path("session-1", "init.mp4").await.unwrap();
        assert_eq!(tokio::fs::read(init).await.unwrap(), b"init");
        manager.stop("session-1").await.unwrap();
        assert!(
            !PathBuf::from(temp_dir.path())
                .join("config/web-playback/session-1")
                .exists()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn emby_seek_uses_distinct_generations_and_supports_backward_restart() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::tempdir().unwrap();
        let script = temp_dir.path().join("fake-ffmpeg-generations");
        let script_body = r##"#!/bin/sh
set -eu
manifest=""
segment=""
start_number=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    -hls_segment_filename) segment="$2"; shift 2 ;;
    -start_number) start_number="$2"; shift 2 ;;
    *.m3u8) manifest="$1"; shift ;;
    *) shift ;;
  esac
done
directory=$(dirname "$manifest")
mkdir -p "$directory"
printf '#EXTM3U\n#EXT-X-MAP:URI="init.mp4"\n#EXTINF:4,\nsegment_%06d.m4s\n' "$start_number" > "$manifest"
printf init > "$directory/init.mp4"
segment_path=$(printf '%s' "$segment" | sed "s/%06d/$(printf '%06d' "$start_number")/")
printf 'segment-%s' "$start_number" > "$segment_path"
while :; do sleep 1; done
"##;
        tokio::fs::write(&script, script_body).await.unwrap();
        let mut permissions = tokio::fs::metadata(&script).await.unwrap().permissions();
        permissions.set_mode(0o700);
        tokio::fs::set_permissions(&script, permissions)
            .await
            .unwrap();
        let manager = super::HlsManager::new_for_tests(
            temp_dir.path().join("config"),
            script.to_string_lossy().into_owned(),
        );

        manager
            .start_emby_vod(
                "emby-generations",
                ServerTier::SoftwareTranscode,
                Path::new("input.mkv"),
                Some(1_000_000),
                Some(600 * 10_000_000),
            )
            .await
            .unwrap();
        let initial_manifest = manager.wait_for_manifest("emby-generations").await.unwrap();
        assert_eq!(
            initial_manifest.file_name().and_then(|name| name.to_str()),
            Some("generation_000000.m3u8")
        );

        let forward = manager
            .wait_for_asset("emby-generations", "segment_000100.m4s")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(forward).await.unwrap(), b"segment-100");
        assert!(initial_manifest.exists());

        let backward = manager
            .wait_for_asset("emby-generations", "segment_000050.m4s")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(backward).await.unwrap(), b"segment-50");
        assert!(initial_manifest.exists());

        let session_directory = manager.session_directory("emby-generations").await.unwrap();
        assert!(session_directory.join("generation_000001.m3u8").exists());
        assert!(session_directory.join("generation_000002.m3u8").exists());

        manager.stop("emby-generations").await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn newer_emby_seek_supersedes_an_old_waiter_without_restarting_back() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::tempdir().unwrap();
        let script = temp_dir.path().join("fake-ffmpeg-concurrent-seeks");
        let script_body = r##"#!/bin/sh
set -eu
manifest=""
segment=""
start_number=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    -hls_segment_filename) segment="$2"; shift 2 ;;
    -start_number) start_number="$2"; shift 2 ;;
    *.m3u8) manifest="$1"; shift ;;
    *) shift ;;
  esac
done
directory=$(dirname "$manifest")
mkdir -p "$directory"
printf '#EXTM3U\n#EXT-X-MAP:URI="init.mp4"\n#EXTINF:4,\nsegment_%06d.m4s\n' "$start_number" > "$manifest"
printf init > "$directory/init.mp4"
if [ "$start_number" -eq 100 ]; then
  sleep 2
fi
segment_path=$(printf '%s' "$segment" | sed "s/%06d/$(printf '%06d' "$start_number")/")
printf 'segment-%s' "$start_number" > "$segment_path"
while :; do sleep 1; done
"##;
        tokio::fs::write(&script, script_body).await.unwrap();
        let mut permissions = tokio::fs::metadata(&script).await.unwrap().permissions();
        permissions.set_mode(0o700);
        tokio::fs::set_permissions(&script, permissions)
            .await
            .unwrap();
        let manager = super::HlsManager::new_for_tests(
            temp_dir.path().join("config"),
            script.to_string_lossy().into_owned(),
        );
        manager
            .start_emby_vod(
                "emby-concurrent-seeks",
                ServerTier::SoftwareTranscode,
                Path::new("input.mkv"),
                Some(1_000_000),
                Some(1_000 * 10_000_000),
            )
            .await
            .unwrap();
        manager
            .wait_for_manifest("emby-concurrent-seeks")
            .await
            .unwrap();

        let old_manager = manager.clone();
        let old_request = tokio::spawn(async move {
            old_manager
                .wait_for_asset("emby-concurrent-seeks", "segment_000100.m4s")
                .await
        });
        let session_directory = manager
            .session_directory("emby-concurrent-seeks")
            .await
            .unwrap();
        for _ in 0..50 {
            if session_directory.join("generation_000001.m3u8").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(session_directory.join("generation_000001.m3u8").exists());

        let newest = manager
            .wait_for_asset("emby-concurrent-seeks", "segment_000200.m4s")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(newest).await.unwrap(), b"segment-200");
        assert!(matches!(
            old_request.await.unwrap(),
            Err(super::HlsError::Superseded)
        ));
        assert!(!session_directory.join("generation_000003.m3u8").exists());

        manager.stop("emby-concurrent-seeks").await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn emby_seek_checks_resource_limits_before_stopping_the_active_generation() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::tempdir().unwrap();
        let script = temp_dir.path().join("fake-ffmpeg-restart-quota");
        let script_body = r##"#!/bin/sh
set -eu
manifest=""
segment=""
start_number=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    -hls_segment_filename) segment="$2"; shift 2 ;;
    -start_number) start_number="$2"; shift 2 ;;
    *.m3u8) manifest="$1"; shift ;;
    *) shift ;;
  esac
done
directory=$(dirname "$manifest")
mkdir -p "$directory"
printf '#EXTM3U\n#EXT-X-MAP:URI="init.mp4"\n#EXTINF:4,\nsegment_%06d.m4s\n' "$start_number" > "$manifest"
printf init > "$directory/init.mp4"
segment_path=$(printf '%s' "$segment" | sed "s/%06d/$(printf '%06d' "$start_number")/")
printf 'segment-%s' "$start_number" > "$segment_path"
while :; do sleep 1; done
"##;
        tokio::fs::write(&script, script_body).await.unwrap();
        let mut permissions = tokio::fs::metadata(&script).await.unwrap().permissions();
        permissions.set_mode(0o700);
        tokio::fs::set_permissions(&script, permissions)
            .await
            .unwrap();
        let mut manager = super::HlsManager::new_for_tests(
            temp_dir.path().join("config"),
            script.to_string_lossy().into_owned(),
        );
        manager
            .start_emby_vod(
                "emby-restart-quota",
                ServerTier::SoftwareTranscode,
                Path::new("input.mkv"),
                Some(1_000_000),
                Some(600 * 10_000_000),
            )
            .await
            .unwrap();
        manager
            .wait_for_manifest("emby-restart-quota")
            .await
            .unwrap();
        let session_directory = manager
            .session_directory("emby-restart-quota")
            .await
            .unwrap();
        manager.min_free_bytes = u64::MAX;
        assert!(matches!(
            manager
                .wait_for_asset("emby-restart-quota", "segment_000100.m4s")
                .await,
            Err(super::HlsError::Limit)
        ));
        assert!(!session_directory.join("generation_000001.m3u8").exists());
        manager.min_free_bytes = 0;

        let oversized = tokio::fs::File::create(session_directory.join("oversized.tmp"))
            .await
            .unwrap();
        oversized
            .set_len(super::MAX_SESSION_BYTES + 1)
            .await
            .unwrap();

        assert!(matches!(
            manager
                .wait_for_asset("emby-restart-quota", "segment_000100.m4s")
                .await,
            Err(super::HlsError::Limit)
        ));
        let first = manager
            .wait_for_asset("emby-restart-quota", "segment_000000.m4s")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(first).await.unwrap(), b"segment-0");
        assert!(!session_directory.join("generation_000001.m3u8").exists());

        manager.stop("emby-restart-quota").await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn manager_stops_the_entire_process_group() {
        use std::{os::unix::fs::PermissionsExt, path::PathBuf};

        let temp_dir = tempfile::tempdir().unwrap();
        let marker = temp_dir.path().join("child-marker");
        let marker_literal = marker.to_string_lossy().replace('\'', "'\\''");
        let script = temp_dir.path().join("fake-ffmpeg-process-group");
        let script_body = format!(
            "#!/bin/sh\nset -eu\nmanifest=\"\"\nsegment=\"\"\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -hls_segment_filename) segment=\"$2\"; shift 2 ;;\n    *.m3u8) manifest=\"$1\"; shift ;;\n    *) shift ;;\n  esac\ndone\ndirectory=$(dirname \"$manifest\")\nmkdir -p \"$directory\"\nprintf '#EXTM3U\\n#EXT-X-MAP:URI=\\\"init.mp4\\\"\\n#EXTINF:1,\\nsegment_000000.m4s\\n' > \"$manifest\"\nprintf init > \"$directory/init.mp4\"\nprintf segment > \"$(printf '%s' \"$segment\" | sed 's/%06d/000000/')\"\n(sleep 0.5; printf child > '{marker_literal}') &\nwhile :; do sleep 1; done\n"
        );
        tokio::fs::write(&script, script_body).await.unwrap();
        let mut permissions = tokio::fs::metadata(&script).await.unwrap().permissions();
        permissions.set_mode(0o700);
        tokio::fs::set_permissions(&script, permissions)
            .await
            .unwrap();
        let manager = super::HlsManager::new_with_executable(
            temp_dir.path().join("config"),
            script.to_string_lossy().into_owned(),
        );

        manager
            .start(
                "process-group",
                ServerTier::Remux,
                Path::new("input.mkv"),
                None,
                None,
                None,
            )
            .await
            .unwrap();
        manager.wait_for_manifest("process-group").await.unwrap();
        manager.stop("process-group").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;

        assert!(!marker.exists());
        assert!(
            !PathBuf::from(temp_dir.path())
                .join("config/web-playback/process-group")
                .exists()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn manager_rejects_new_sessions_below_the_free_space_watermark() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = super::HlsManager::new_with_limits(
            temp_dir.path().join("config"),
            "/bin/true".to_owned(),
            u64::MAX,
        );

        let error = manager
            .start(
                "low-space",
                ServerTier::Remux,
                Path::new("input.mkv"),
                None,
                None,
                None,
            )
            .await
            .unwrap_err();

        assert!(matches!(error, super::HlsError::Limit));
        assert!(
            !temp_dir
                .path()
                .join("config/web-playback/low-space")
                .exists()
        );
    }
}
