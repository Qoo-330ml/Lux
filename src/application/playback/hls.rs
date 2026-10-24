use std::{
    collections::HashMap,
    fmt,
    fmt::Write as _,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};

use tokio::{
    fs,
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::{Child, Command},
    sync::{Mutex, OwnedSemaphorePermit, RwLock, Semaphore},
    time::sleep,
};

use super::decision::ServerTier;

const MAX_REMUX_SESSIONS: usize = 4;
const MAX_HARDWARE_SESSIONS: usize = 2;
const MAX_SOFTWARE_SESSIONS: usize = 1;
const MAX_SESSION_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const DEFAULT_MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;
/// Default cap for all Web HLS session directories together.
///
/// Four sessions of the per-session ceiling fit inside it, so the disk cannot be filled by
/// playback even when every transcode slot is busy. Override with `LUX_HLS_GLOBAL_DISK_BUDGET_BYTES`.
const DEFAULT_MAX_GLOBAL_SESSION_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MANIFEST_WAIT_ATTEMPTS: usize = 50;
const HLS_ASSET_WAIT_ATTEMPTS: usize = 600;
const HLS_SEGMENT_DURATION_TICKS: i64 = 4 * 10_000_000;
const HLS_SEGMENT_RESTART_GAP: i64 = 6;
/// Upper bound for the per-session segment-to-generation map.
const HLS_MAX_TRACKED_SEGMENTS: usize = 4_096;
const MAX_EMBY_VOD_SEGMENTS: i64 = 1_000_000;
const SESSION_QUOTA_CACHE_TTL: Duration = Duration::from_secs(5);
/// Upper bound for inspecting the process table while reclaiming orphans at startup.
const ORPHAN_PROCESS_SCAN_TIMEOUT: Duration = Duration::from_secs(5);
/// Session identifiers become directory names, so they stay within one predictable segment.
const MAX_SESSION_ID_BYTES: usize = 128;
/// `ffmpeg -progress` writes one `key=value` pair per line; anything larger is not a progress
/// report and must not grow the reader's buffer.
const MAX_PROGRESS_LINE_BYTES: usize = 4_096;

/// The newest complete-enough `ffmpeg -progress` values for one HLS process generation.
///
/// FFmpeg emits a block of `key=value` lines roughly twice a second and closes each block with
/// `progress=continue` or `progress=end`. The reader applies lines as they arrive, so a caller can
/// observe a half-updated block; every field is independently optional and display-only.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HlsProgressSnapshot {
    pub(crate) generation: u64,
    pub(crate) frame: Option<u64>,
    pub(crate) fps: Option<f64>,
    /// Encoded output bitrate in kbit/s, as reported by FFmpeg.
    pub(crate) bitrate_kbps: Option<f64>,
    pub(crate) total_size: Option<u64>,
    pub(crate) out_time_us: Option<u64>,
    pub(crate) speed: Option<f64>,
    pub(crate) duplicated_frames: Option<u64>,
    pub(crate) dropped_frames: Option<u64>,
    pub(crate) finished: bool,
    /// Milliseconds since the Unix epoch of the last accepted line; zero means "no report yet".
    pub(crate) updated_at_ms: i64,
}

impl HlsProgressSnapshot {
    /// Applies one `key=value` progress line. Returns whether the line carried a known value.
    fn apply_line(&mut self, line: &str, now_ms: i64) -> bool {
        let Some((key, value)) = line.trim().split_once('=') else {
            return false;
        };
        let value = value.trim();
        let changed = match key {
            "frame" => assign_parsed(&mut self.frame, parse_u64(value)),
            "fps" => assign_parsed(&mut self.fps, parse_f64(value)),
            "bitrate" => assign_parsed(&mut self.bitrate_kbps, parse_bitrate_kbps(value)),
            "total_size" => assign_parsed(&mut self.total_size, parse_u64(value)),
            "out_time_us" => assign_parsed(&mut self.out_time_us, parse_u64(value)),
            "speed" => assign_parsed(&mut self.speed, parse_speed(value)),
            "dup_frames" => assign_parsed(&mut self.duplicated_frames, parse_u64(value)),
            "drop_frames" => assign_parsed(&mut self.dropped_frames, parse_u64(value)),
            "progress" => {
                self.finished = value == "end";
                true
            }
            // `out_time_ms` is documented as microseconds in practice, so it is ignored in
            // favour of `out_time_us`; every other key (stream_*_q, out_time, ...) is unused.
            _ => false,
        };
        if changed {
            self.updated_at_ms = now_ms;
        }
        changed
    }
}

fn assign_parsed<T>(slot: &mut Option<T>, value: Option<T>) -> bool {
    match value {
        Some(value) => {
            *slot = Some(value);
            true
        }
        None => false,
    }
}

fn parse_u64(value: &str) -> Option<u64> {
    value.parse::<u64>().ok()
}

fn parse_f64(value: &str) -> Option<f64> {
    value
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn parse_bitrate_kbps(value: &str) -> Option<f64> {
    let value = value.strip_suffix("kbits/s")?.trim();
    parse_f64(value)
}

fn parse_speed(value: &str) -> Option<f64> {
    let value = value.strip_suffix('x')?.trim();
    parse_f64(value)
}

fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HlsSegmentContainer {
    MpegTs,
    FragmentedMp4,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct HlsStartOptions {
    pub(crate) segment_container: HlsSegmentContainer,
    pub(crate) start_time_ticks: Option<i64>,
    pub(crate) runtime_ticks: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct HlsWebStartOptions {
    pub(crate) video_bitrate: Option<i64>,
    pub(crate) start_time_ticks: Option<i64>,
    pub(crate) runtime_ticks: Option<i64>,
    pub(crate) audio_stream_index: Option<i64>,
}

impl HlsSegmentContainer {
    pub(crate) const fn segment_extension(self) -> &'static str {
        match self {
            Self::MpegTs => "ts",
            Self::FragmentedMp4 => "m4s",
        }
    }

    pub(crate) const fn mime_type(self) -> &'static str {
        match self {
            Self::MpegTs => "video/mp2t",
            Self::FragmentedMp4 => "video/mp4",
        }
    }

    pub(crate) const fn emby_container(self) -> &'static str {
        match self {
            Self::MpegTs => "ts",
            Self::FragmentedMp4 => "mp4",
        }
    }

    const fn uses_initialization_segment(self) -> bool {
        matches!(self, Self::FragmentedMp4)
    }
}

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
    progress: Arc<Mutex<HlsProgressSnapshot>>,
}

struct HlsProcessSpecification {
    input: PathBuf,
    tier: ServerTier,
    video_bitrate: Option<i64>,
    audio_stream_index: Option<i64>,
    emby_vod: bool,
    segment_container: HlsSegmentContainer,
}

/// Remembers which generation served a segment without letting the map grow without bound.
fn remember_segment_generation(state: &mut HlsProcessState, segment: i64, generation: u64) {
    if state.emby_segment_generations.len() >= HLS_MAX_TRACKED_SEGMENTS {
        // The map only avoids needless restarts; dropping it costs at most one extra restart.
        state.emby_segment_generations.clear();
    }
    state.emby_segment_generations.insert(segment, generation);
}

struct HlsProcessState {
    child: Option<Child>,
    generation: u64,
    emby_segment_generations: HashMap<i64, u64>,
    init: PathBuf,
    manifest: PathBuf,
    permit: Option<OwnedSemaphorePermit>,
    segment_start_number: i64,
}

struct SegmentSchedule {
    generation: u64,
    restarted: bool,
}

struct HlsOutput<'a> {
    init_file_name: &'a str,
    manifest: &'a Path,
    segment_file_name: String,
    segment_container: HlsSegmentContainer,
    preserve_input_timestamps: bool,
}

#[derive(Clone, Copy)]
struct FfmpegTimelineOptions {
    start_time_ticks: Option<i64>,
    audio_stream_index: Option<i64>,
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
    processes: Arc<RwLock<HashMap<String, Arc<HlsProcess>>>>,
    remux_slots: Arc<Semaphore>,
    hardware_slots: Arc<Semaphore>,
    software_slots: Arc<Semaphore>,
    hardware_encoder: Option<String>,
    ffmpeg_executable: String,
    min_free_bytes: u64,
    max_global_session_bytes: u64,
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

    #[cfg(test)]
    fn with_global_session_budget(mut self, bytes: u64) -> Self {
        self.max_global_session_bytes = bytes;
        self
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
            processes: Arc::new(RwLock::new(HashMap::new())),
            remux_slots: Arc::new(Semaphore::new(MAX_REMUX_SESSIONS)),
            hardware_slots: Arc::new(Semaphore::new(MAX_HARDWARE_SESSIONS)),
            software_slots: Arc::new(Semaphore::new(MAX_SOFTWARE_SESSIONS)),
            hardware_encoder,
            ffmpeg_executable,
            min_free_bytes,
            max_global_session_bytes: global_session_budget_from_env()
                .unwrap_or(DEFAULT_MAX_GLOBAL_SESSION_BYTES),
        }
    }

    pub(crate) fn hardware_transcode_available(&self) -> bool {
        self.hardware_encoder.is_some()
    }

    pub(crate) fn with_executable(mut self, executable: String) -> Self {
        self.ffmpeg_executable = executable;
        self
    }

    #[cfg(test)]
    pub(crate) async fn start(
        &self,
        session_id: &str,
        tier: ServerTier,
        input: &Path,
        video_bitrate: Option<i64>,
        start_time_ticks: Option<i64>,
        runtime_ticks: Option<i64>,
    ) -> Result<(), HlsError> {
        self.start_with_audio(
            session_id,
            tier,
            input,
            HlsWebStartOptions {
                video_bitrate,
                start_time_ticks,
                runtime_ticks,
                audio_stream_index: None,
            },
        )
        .await
    }

    pub(crate) async fn start_with_audio(
        &self,
        session_id: &str,
        tier: ServerTier,
        input: &Path,
        options: HlsWebStartOptions,
    ) -> Result<(), HlsError> {
        self.start_process(
            session_id,
            HlsProcessSpecification {
                input: input.to_path_buf(),
                tier,
                video_bitrate: options.video_bitrate,
                audio_stream_index: options.audio_stream_index,
                emby_vod: false,
                segment_container: HlsSegmentContainer::FragmentedMp4,
            },
            options.start_time_ticks,
            options.runtime_ticks,
            true,
        )
        .await
    }

    pub(crate) async fn start_emby_vod(
        &self,
        session_id: &str,
        tier: ServerTier,
        input: &Path,
        video_bitrate: Option<i64>,
        options: HlsStartOptions,
    ) -> Result<(), HlsError> {
        self.start_process(
            session_id,
            HlsProcessSpecification {
                input: input.to_path_buf(),
                tier,
                video_bitrate,
                audio_stream_index: None,
                emby_vod: true,
                segment_container: options.segment_container,
            },
            options.start_time_ticks,
            options.runtime_ticks,
            false,
        )
        .await
    }

    async fn start_process(
        &self,
        session_id: &str,
        specification: HlsProcessSpecification,
        start_time_ticks: Option<i64>,
        runtime_ticks: Option<i64>,
        spawn_immediately: bool,
    ) -> Result<(), HlsError> {
        let permit = if spawn_immediately {
            Some(self.acquire_permit(specification.tier)?)
        } else {
            None
        };
        fs::create_dir_all(&self.base_directory)
            .await
            .map_err(HlsError::Io)?;
        if !has_sufficient_free_space(&self.base_directory, self.min_free_bytes).await {
            return Err(HlsError::Limit);
        }
        if !self.session_bytes_within_global_budget().await? {
            tracing::warn!(
                budget_bytes = self.max_global_session_bytes,
                session_id = %session_id,
                "refusing to start Web HLS: the global disk budget is exhausted"
            );
            return Err(HlsError::Limit);
        }
        let directory = self.validated_session_directory(session_id)?;
        fs::create_dir_all(&directory).await.map_err(HlsError::Io)?;
        let segment_start_number = hls_start_number(start_time_ticks);
        let generation = 0;
        let progress = Arc::new(Mutex::new(HlsProgressSnapshot {
            generation,
            ..HlsProgressSnapshot::default()
        }));
        let init = hls_init_path(
            &directory,
            specification.emby_vod,
            generation,
            specification.segment_container,
        );
        let manifest = hls_manifest_path(&directory, specification.emby_vod, generation);
        let child = if spawn_immediately {
            match self.spawn_child(
                &specification,
                &directory,
                &init,
                &manifest,
                generation,
                start_time_ticks,
                Arc::clone(&progress),
            ) {
                Ok(child) => Some(child),
                Err(error) => {
                    let _ = fs::remove_dir_all(&directory).await;
                    return Err(error);
                }
            }
        } else {
            None
        };
        let process = Arc::new(HlsProcess {
            directory,
            initial_manifest: manifest.clone(),
            runtime_ticks: runtime_ticks.filter(|ticks| *ticks > 0),
            specification,
            state: Mutex::new(HlsProcessState {
                child,
                generation,
                emby_segment_generations: HashMap::new(),
                init,
                manifest,
                permit,
                segment_start_number,
            }),
            restart: Mutex::new(()),
            quota: Mutex::new(SessionQuotaCache::default()),
            progress,
        });
        let previous = self
            .processes
            .write()
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
        init: &Path,
        manifest: &Path,
        generation: u64,
        start_time_ticks: Option<i64>,
        progress: Arc<Mutex<HlsProgressSnapshot>>,
    ) -> Result<Child, HlsError> {
        let init_file_name = init.file_name().and_then(|name| name.to_str());
        let args = if specification.emby_vod {
            ffmpeg_args_for_output_with_audio(
                &specification.input,
                specification.tier,
                self.hardware_encoder.as_deref(),
                specification.video_bitrate,
                FfmpegTimelineOptions {
                    start_time_ticks,
                    audio_stream_index: specification.audio_stream_index,
                    preserve_input_timestamps: true,
                },
                HlsOutput {
                    init_file_name: init_file_name.unwrap_or("init.mp4"),
                    manifest,
                    segment_file_name: hls_segment_pattern(
                        directory,
                        true,
                        generation,
                        specification.segment_container,
                    )
                    .to_string_lossy()
                    .into_owned(),
                    segment_container: specification.segment_container,
                    preserve_input_timestamps: true,
                },
            )?
        } else {
            ffmpeg_args_with_audio(
                &specification.input,
                directory,
                specification.tier,
                self.hardware_encoder.as_deref(),
                specification.video_bitrate,
                start_time_ticks,
                specification.audio_stream_index,
            )?
        };
        let mut command = Command::new(&self.ffmpeg_executable);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
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
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                let mut line = Vec::new();
                loop {
                    line.clear();
                    // Bound each read so a malformed producer cannot grow this buffer without
                    // limit; `read_until` stops at the newline or at `MAX_PROGRESS_LINE_BYTES`.
                    let read = match (&mut reader)
                        .take(MAX_PROGRESS_LINE_BYTES as u64)
                        .read_until(b'\n', &mut line)
                        .await
                    {
                        Ok(read) => read,
                        Err(_) => break,
                    };
                    if read == 0 {
                        break;
                    }
                    let Ok(text) = std::str::from_utf8(&line) else {
                        continue;
                    };
                    let now_ms = unix_millis();
                    let mut snapshot = progress.lock().await;
                    if snapshot.generation != generation {
                        *snapshot = HlsProgressSnapshot {
                            generation,
                            ..HlsProgressSnapshot::default()
                        };
                    }
                    snapshot.apply_line(text, now_ms);
                }
            });
        }
        Ok(child)
    }

    /// Latest FFmpeg progress for one HLS session, or `None` before FFmpeg reported anything.
    pub(crate) async fn progress(&self, session_id: &str) -> Option<HlsProgressSnapshot> {
        let process = self.processes.read().await.get(session_id).cloned()?;
        let snapshot = process.progress.lock().await.clone();
        (snapshot.updated_at_ms > 0).then_some(snapshot)
    }

    pub(crate) async fn start_for_initial_asset(
        &self,
        session_id: &str,
        asset: Option<&str>,
    ) -> Result<(), HlsError> {
        let process = self
            .processes
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        if let Some(asset) = asset
            && !is_valid_asset(asset, process.specification.segment_container)
        {
            return Err(HlsError::InvalidAsset);
        }
        if process.state.lock().await.child.is_some() {
            return Ok(());
        }
        let requested_segment = match asset {
            Some(asset) if asset_segment_number(asset)?.is_some() => {
                asset_segment_number(asset)?.unwrap_or(0)
            }
            Some(asset) if emby_init_segment_number(asset).is_some() => {
                emby_init_segment_number(asset).unwrap_or(0)
            }
            _ => process.state.lock().await.segment_start_number,
        };
        let requested_path = match asset {
            Some(asset)
                if is_emby_generation_asset(asset, process.specification.segment_container) =>
            {
                process.directory.join(asset)
            }
            Some("init.mp4") => emby_asset_path(&process, "init.mp4").await,
            Some(asset)
                if asset_segment_number(asset)?.is_some()
                    || emby_init_segment_number(asset).is_some() =>
            {
                emby_asset_path(&process, asset).await
            }
            _ => process.directory.join(format!(
                "segment_{requested_segment:06}.{}",
                process.specification.segment_container.segment_extension()
            )),
        };
        self.restart_for_segment(session_id, &process, requested_segment, &requested_path)
            .await?;
        Ok(())
    }

    pub(crate) async fn wait_for_manifest(&self, session_id: &str) -> Result<PathBuf, HlsError> {
        let process = self
            .processes
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        let manifest = process.initial_manifest.clone();
        if process.specification.emby_vod && fs::metadata(&manifest).await.is_err() {
            let initial_segment = process.state.lock().await.segment_start_number;
            let initial_segment_path = process.directory.join(format!(
                "segment_{initial_segment:06}.{}",
                process.specification.segment_container.segment_extension()
            ));
            self.restart_for_segment(session_id, &process, initial_segment, &initial_segment_path)
                .await?;
        }
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
        let process = self
            .processes
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        if !is_valid_asset(asset, process.specification.segment_container) {
            return Err(HlsError::InvalidAsset);
        }
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
        let segment_number = asset_segment_number(asset)?;
        let process = self
            .processes
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        if !is_valid_asset(asset, process.specification.segment_container) {
            return Err(HlsError::InvalidAsset);
        }
        let is_emby_init = process.specification.emby_vod
            && process
                .specification
                .segment_container
                .uses_initialization_segment()
            && (asset == "init.mp4" || emby_init_segment_number(asset).is_some());
        let public_path = process.directory.join(asset);
        if !public_path.starts_with(&process.directory) {
            return Err(HlsError::InvalidAsset);
        }
        let generation_segment = if process.specification.emby_vod
            && !is_emby_generation_asset(asset, process.specification.segment_container)
        {
            match (segment_number, emby_init_segment_number(asset)) {
                (Some(segment_number), _) | (None, Some(segment_number)) => Some(segment_number),
                (None, None) if is_emby_init => {
                    Some(process.state.lock().await.segment_start_number)
                }
                (None, None) => None,
            }
        } else {
            None
        };
        let initial_path = emby_asset_path(&process, asset).await;
        let mut schedule = if let Some(segment_number) = generation_segment {
            Some(
                self.restart_for_segment(session_id, &process, segment_number, &initial_path)
                    .await?,
            )
        } else {
            None
        };
        for _ in 0..HLS_ASSET_WAIT_ATTEMPTS {
            let path = if let Some(schedule) = schedule.as_ref() {
                emby_asset_path_for_generation(&process, asset, schedule.generation)
            } else {
                emby_asset_path(&process, asset).await
            };
            if fs::metadata(&path)
                .await
                .is_ok_and(|metadata| metadata.is_file())
            {
                return Ok(path);
            }
            let current_process = self
                .processes
                .read()
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
            if generation_segment.is_some()
                && schedule
                    .as_ref()
                    .is_some_and(|scheduled| scheduled.generation != generation)
            {
                return Err(HlsError::Superseded);
            }
            if finished {
                if let Some(segment_number) = generation_segment
                    && schedule
                        .as_ref()
                        .is_some_and(|scheduled| !scheduled.restarted)
                {
                    let path = emby_asset_path_for_generation(
                        &process,
                        asset,
                        process.state.lock().await.generation,
                    );
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
            .read()
            .await
            .get(session_id)
            .is_some_and(|current| Arc::ptr_eq(current, process));
        if !is_current_process {
            return Err(HlsError::NotFound);
        }
        let mut state = process.state.lock().await;
        let initial_start = state.child.is_none();
        let latest_segment = latest_manifest_segment_number(&state.manifest).await?;
        let finished = match state.child.as_mut() {
            Some(child) => child.try_wait().map_err(HlsError::Io)?.is_some(),
            None => true,
        };
        let current_segment =
            latest_segment.unwrap_or_else(|| state.segment_start_number.saturating_sub(1));
        if process.specification.emby_vod {
            let is_init_asset = process
                .specification
                .segment_container
                .uses_initialization_segment()
                && requested_path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.ends_with("_init.mp4") || value == "init.mp4");
            if let Some(bound_generation) = state
                .emby_segment_generations
                .get(&requested_segment)
                .copied()
            {
                if bound_generation == state.generation && !finished {
                    return Ok(SegmentSchedule {
                        generation: bound_generation,
                        restarted: false,
                    });
                }
                if bound_generation != state.generation {
                    let bound_path = emby_generation_asset_path(
                        process,
                        bound_generation,
                        requested_segment,
                        is_init_asset,
                    );
                    if fs::metadata(bound_path)
                        .await
                        .is_ok_and(|metadata| metadata.is_file())
                    {
                        return Ok(SegmentSchedule {
                            generation: bound_generation,
                            restarted: false,
                        });
                    }
                    return Err(HlsError::Superseded);
                }
                state.emby_segment_generations.remove(&requested_segment);
            }
        }
        let request_is_near_current_generation = requested_segment >= state.segment_start_number
            && requested_segment.saturating_sub(current_segment) <= HLS_SEGMENT_RESTART_GAP;
        if request_is_near_current_generation
            && fs::metadata(requested_path)
                .await
                .is_ok_and(|metadata| metadata.is_file())
        {
            if process.specification.emby_vod {
                let generation = state.generation;
                remember_segment_generation(&mut state, requested_segment, generation);
            }
            return Ok(SegmentSchedule {
                generation: state.generation,
                restarted: false,
            });
        }
        let restart_reason = if initial_start {
            Some("initial_request")
        } else if finished {
            Some("process_finished")
        } else if requested_segment < state.segment_start_number {
            Some("backward_seek")
        } else if requested_segment.saturating_sub(current_segment) > HLS_SEGMENT_RESTART_GAP {
            Some("forward_seek")
        } else {
            None
        };
        let Some(restart_reason) = restart_reason else {
            if process.specification.emby_vod {
                let generation = state.generation;
                remember_segment_generation(&mut state, requested_segment, generation);
            }
            return Ok(SegmentSchedule {
                generation: state.generation,
                restarted: false,
            });
        };

        let previous_init = state.init.clone();
        let wait_for_previous_init = process.specification.emby_vod && !initial_start && !finished;
        if wait_for_previous_init {
            drop(state);
            let _ = wait_for_file(&previous_init, 20).await;
            state = process.state.lock().await;
        }

        self.check_restart_resources(process).await?;
        let permit = if initial_start {
            Some(self.acquire_permit(process.specification.tier)?)
        } else {
            None
        };
        stop_child(&mut state.child).await;
        let start_time_ticks = requested_segment
            .checked_mul(HLS_SEGMENT_DURATION_TICKS)
            .ok_or(HlsError::InvalidAsset)?;
        let generation = if initial_start {
            state.generation
        } else {
            state.generation.saturating_add(1)
        };
        let manifest = if initial_start {
            state.manifest.clone()
        } else {
            hls_manifest_path(&process.directory, true, generation)
        };
        let init = hls_init_path(
            &process.directory,
            true,
            generation,
            process.specification.segment_container,
        );
        let child = self.spawn_child(
            &process.specification,
            &process.directory,
            &init,
            &manifest,
            generation,
            Some(start_time_ticks),
            Arc::clone(&process.progress),
        )?;
        tracing::info!(
            event = "emby_hls_segment_restart",
            session_id_prefix = %session_id.chars().take(8).collect::<String>(),
            segment_number = requested_segment,
            current_segment_number = current_segment,
            reason = restart_reason,
            "started Emby HLS transcoding at requested segment"
        );
        state.child = Some(child);
        state.generation = generation;
        state.init = init;
        state.manifest = manifest;
        if let Some(permit) = permit {
            state.permit = Some(permit);
        }
        state.segment_start_number = requested_segment;
        if process.specification.emby_vod {
            state
                .emby_segment_generations
                .insert(requested_segment, generation);
        }
        Ok(SegmentSchedule {
            generation,
            restarted: true,
        })
    }

    async fn check_restart_resources(&self, process: &HlsProcess) -> Result<(), HlsError> {
        if !has_sufficient_free_space(&self.base_directory, self.min_free_bytes).await {
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
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        let Some(runtime_ticks) = process.runtime_ticks else {
            return Ok(None);
        };
        Ok(vod_manifest(
            runtime_ticks,
            process.specification.segment_container,
        ))
    }

    pub(crate) async fn session_directory(&self, session_id: &str) -> Result<PathBuf, HlsError> {
        let process = self
            .processes
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        Ok(process.directory.clone())
    }

    pub(crate) async fn has_started(&self, session_id: &str) -> Result<bool, HlsError> {
        let process = self
            .processes
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        let started = process.state.lock().await.child.is_some();
        Ok(started)
    }

    pub(crate) async fn segment_container(
        &self,
        session_id: &str,
    ) -> Result<HlsSegmentContainer, HlsError> {
        let process = self
            .processes
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        Ok(process.specification.segment_container)
    }

    pub(crate) async fn within_quota(&self, session_id: &str) -> Result<bool, HlsError> {
        let process = self
            .processes
            .read()
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

    /// Resolves the per-session directory without ever escaping the HLS root.
    ///
    /// Session identifiers arrive from HTTP paths and Emby playback callbacks, so they are
    /// validated before they are joined into a path that later gets recursively deleted.
    fn validated_session_directory(&self, session_id: &str) -> Result<PathBuf, HlsError> {
        if !is_safe_session_id(session_id) {
            return Err(HlsError::NotFound);
        }
        let directory = self.base_directory.join(session_id);
        if directory.parent() != Some(self.base_directory.as_path()) {
            return Err(HlsError::NotFound);
        }
        Ok(directory)
    }

    pub(crate) async fn stop(&self, session_id: &str) -> Result<(), HlsError> {
        let directory = self.validated_session_directory(session_id)?;
        let process = self.processes.write().await.remove(session_id);
        if let Some(process) = process {
            stop_process(process).await;
        } else if fs::metadata(&directory).await.is_ok() {
            fs::remove_dir_all(directory).await.map_err(HlsError::Io)?;
        }
        Ok(())
    }

    pub(crate) async fn cleanup_orphans(&self) -> Result<(), HlsError> {
        self.terminate_orphan_processes().await;
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

    /// Kills the ffmpeg children a crashed process left behind.
    ///
    /// Startup drops every session directory, but an ffmpeg process started by a previous
    /// incarnation keeps encoding into its deleted output paths, burning CPU until the host is
    /// rebooted. Session ids are regenerated per process, so any process whose command line
    /// mentions both our ffmpeg executable and the session directory base at startup is stale.
    async fn terminate_orphan_processes(&self) {
        let executable = escape_pgrep_pattern(&self.ffmpeg_executable);
        let base_directory = escape_pgrep_pattern(&self.base_directory.to_string_lossy().as_ref());
        let (Some(by_executable), Some(by_directory)) = (
            process_ids_matching(&executable).await,
            process_ids_matching(&base_directory).await,
        ) else {
            tracing::warn!(
                "could not inspect the process table for orphaned Web HLS ffmpeg processes"
            );
            return;
        };
        let own_pid = std::process::id();
        let orphans = by_executable
            .intersection(&by_directory)
            .copied()
            .filter(|pid| *pid != own_pid)
            .collect::<Vec<_>>();
        for pid in &orphans {
            #[cfg(unix)]
            if let Ok(pid) = libc::pid_t::try_from(*pid) {
                // The process is unrelated to this incarnation, so it cannot be reaped normally.
                let _ = unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
        if !orphans.is_empty() {
            tracing::warn!(
                processes = orphans.len(),
                "terminated orphaned Web HLS ffmpeg processes"
            );
        }
    }

    /// Checks that the existing session directories still fit the global disk budget.
    ///
    /// Uses the cached per-session quota totals when they are fresh and walks the session
    /// directory otherwise, so admitting a playback start never walks every segment on the hot
    /// path. Existing sessions are never evicted: a new start is refused instead.
    async fn session_bytes_within_global_budget(&self) -> Result<bool, HlsError> {
        let processes = self
            .processes
            .read()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let now = Instant::now();
        let mut total_bytes = 0_u64;
        for process in processes {
            let cached = {
                let quota = process.quota.lock().await;
                quota.is_fresh(now).then_some(quota.total_bytes)
            };
            let bytes = match cached {
                Some(bytes) => bytes,
                None => {
                    let bytes = match session_directory_bytes(&process.directory).await {
                        Ok(bytes) => bytes,
                        // A registered process whose directory is already gone holds no space.
                        Err(HlsError::Io(error))
                            if error.kind() == std::io::ErrorKind::NotFound =>
                        {
                            0
                        }
                        Err(error) => return Err(error),
                    };
                    let mut quota = process.quota.lock().await;
                    quota.total_bytes = bytes;
                    quota.checked_at = Some(Instant::now());
                    bytes
                }
            };
            total_bytes = total_bytes.saturating_add(bytes);
            if total_bytes >= self.max_global_session_bytes {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn acquire_permit(&self, tier: ServerTier) -> Result<OwnedSemaphorePermit, HlsError> {
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
    let directory = directory.to_owned();
    // One blocking hop for the whole walk: per-entry `tokio::fs` calls would schedule a
    // separate blocking task for every segment file on every quota check.
    let total = tokio::task::spawn_blocking(move || -> std::io::Result<u64> {
        let mut total = 0_u64;
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                total = total.saturating_add(entry.metadata()?.len());
                if total > MAX_SESSION_BYTES {
                    break;
                }
            }
        }
        Ok(total)
    })
    .await
    .map_err(|error| HlsError::Io(std::io::Error::other(error)))?
    .map_err(HlsError::Io)?;
    Ok(total)
}

async fn wait_for_file(path: &Path, attempts: usize) -> bool {
    for _ in 0..attempts {
        if fs::metadata(path)
            .await
            .is_ok_and(|metadata| metadata.is_file())
        {
            return true;
        }
        sleep(Duration::from_millis(100)).await;
    }
    false
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

async fn emby_asset_path(process: &HlsProcess, asset: &str) -> PathBuf {
    if !process.specification.emby_vod {
        return process.directory.join(asset);
    }
    if is_emby_generation_asset(asset, process.specification.segment_container) {
        return process.directory.join(asset);
    }
    let state = process.state.lock().await;
    emby_asset_path_for_generation(process, asset, state.generation)
}

fn emby_asset_path_for_generation(process: &HlsProcess, asset: &str, generation: u64) -> PathBuf {
    if is_emby_generation_asset(asset, process.specification.segment_container) {
        return process.directory.join(asset);
    }
    if process
        .specification
        .segment_container
        .uses_initialization_segment()
        && (asset == "init.mp4" || emby_init_segment_number(asset).is_some())
    {
        hls_init_path(
            &process.directory,
            true,
            generation,
            process.specification.segment_container,
        )
    } else if let Ok(Some(segment_number)) = asset_segment_number(asset) {
        hls_segment_path(
            &process.directory,
            true,
            generation,
            segment_number,
            process.specification.segment_container,
        )
    } else {
        process.directory.join(asset)
    }
}

fn emby_generation_asset_path(
    process: &HlsProcess,
    generation: u64,
    segment_number: i64,
    is_init: bool,
) -> PathBuf {
    if is_init
        && process
            .specification
            .segment_container
            .uses_initialization_segment()
    {
        hls_init_path(
            &process.directory,
            true,
            generation,
            process.specification.segment_container,
        )
    } else {
        hls_segment_path(
            &process.directory,
            true,
            generation,
            segment_number,
            process.specification.segment_container,
        )
    }
}

fn hls_manifest_path(directory: &Path, emby_vod: bool, generation: u64) -> PathBuf {
    if emby_vod {
        directory.join(format!("generation_{generation:06}.m3u8"))
    } else {
        directory.join("index.m3u8")
    }
}

fn hls_init_path(
    directory: &Path,
    emby_vod: bool,
    generation: u64,
    segment_container: HlsSegmentContainer,
) -> PathBuf {
    if emby_vod && segment_container.uses_initialization_segment() {
        directory.join(format!("generation_{generation:06}_init.mp4"))
    } else {
        directory.join("init.mp4")
    }
}

fn hls_segment_pattern(
    directory: &Path,
    emby_vod: bool,
    generation: u64,
    segment_container: HlsSegmentContainer,
) -> PathBuf {
    let extension = segment_container.segment_extension();
    if emby_vod {
        directory.join(format!(
            "generation_{generation:06}_segment_%06d.{extension}"
        ))
    } else {
        directory.join(format!("segment_%06d.{extension}"))
    }
}

fn hls_segment_path(
    directory: &Path,
    emby_vod: bool,
    generation: u64,
    segment_number: i64,
    segment_container: HlsSegmentContainer,
) -> PathBuf {
    if emby_vod {
        directory.join(format!(
            "generation_{generation:06}_segment_{segment_number:06}.{}",
            segment_container.segment_extension()
        ))
    } else {
        directory.join(format!(
            "segment_{segment_number:06}.{}",
            segment_container.segment_extension()
        ))
    }
}

async fn stop_process(process: Arc<HlsProcess>) {
    let _restart = process.restart.lock().await;
    let mut state = process.state.lock().await;
    stop_child(&mut state.child).await;
    state.permit.take();
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

fn global_session_budget_from_env() -> Option<u64> {
    std::env::var("LUX_HLS_GLOBAL_DISK_BUDGET_BYTES")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|bytes| *bytes > 0)
}

/// Returns the pids whose command line matches `pattern`, or `None` when the process table cannot
/// be inspected at all.
async fn process_ids_matching(pattern: &str) -> Option<std::collections::HashSet<u32>> {
    let output = tokio::time::timeout(
        ORPHAN_PROCESS_SCAN_TIMEOUT,
        Command::new("pgrep").arg("-f").arg(pattern).output(),
    )
    .await
    .ok()?
    .ok()?;
    // `pgrep` exits with 1 when nothing matches, which is not an inspection failure.
    if !output.status.success() && output.status.code() != Some(1) {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .collect(),
    )
}

fn escape_pgrep_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(
            character,
            '.' | '\\' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[cfg(test)]
fn ffmpeg_args(
    input: &Path,
    directory: &Path,
    tier: ServerTier,
    hardware_encoder: Option<&str>,
    video_bitrate: Option<i64>,
    start_time_ticks: Option<i64>,
) -> Result<Vec<String>, HlsError> {
    ffmpeg_args_with_audio(
        input,
        directory,
        tier,
        hardware_encoder,
        video_bitrate,
        start_time_ticks,
        None,
    )
}

fn ffmpeg_args_with_audio(
    input: &Path,
    directory: &Path,
    tier: ServerTier,
    hardware_encoder: Option<&str>,
    video_bitrate: Option<i64>,
    start_time_ticks: Option<i64>,
    audio_stream_index: Option<i64>,
) -> Result<Vec<String>, HlsError> {
    ffmpeg_args_with_timeline_audio(
        input,
        directory,
        tier,
        hardware_encoder,
        video_bitrate,
        FfmpegTimelineOptions {
            start_time_ticks,
            audio_stream_index,
            preserve_input_timestamps: false,
        },
    )
}

#[cfg(test)]
fn ffmpeg_args_with_timeline(
    input: &Path,
    directory: &Path,
    tier: ServerTier,
    hardware_encoder: Option<&str>,
    video_bitrate: Option<i64>,
    start_time_ticks: Option<i64>,
    preserve_input_timestamps: bool,
) -> Result<Vec<String>, HlsError> {
    ffmpeg_args_with_timeline_audio(
        input,
        directory,
        tier,
        hardware_encoder,
        video_bitrate,
        FfmpegTimelineOptions {
            start_time_ticks,
            audio_stream_index: None,
            preserve_input_timestamps,
        },
    )
}

fn ffmpeg_args_with_timeline_audio(
    input: &Path,
    directory: &Path,
    tier: ServerTier,
    hardware_encoder: Option<&str>,
    video_bitrate: Option<i64>,
    timing: FfmpegTimelineOptions,
) -> Result<Vec<String>, HlsError> {
    let manifest = directory.join("index.m3u8");
    ffmpeg_args_for_output_with_audio(
        input,
        tier,
        hardware_encoder,
        video_bitrate,
        timing,
        HlsOutput {
            init_file_name: "init.mp4",
            manifest: &manifest,
            segment_file_name: directory
                .join("segment_%06d.m4s")
                .to_string_lossy()
                .into_owned(),
            segment_container: HlsSegmentContainer::FragmentedMp4,
            preserve_input_timestamps: timing.preserve_input_timestamps,
        },
    )
}

#[cfg(test)]
fn ffmpeg_args_for_output(
    input: &Path,
    _directory: &Path,
    tier: ServerTier,
    hardware_encoder: Option<&str>,
    video_bitrate: Option<i64>,
    start_time_ticks: Option<i64>,
    output: HlsOutput<'_>,
) -> Result<Vec<String>, HlsError> {
    ffmpeg_args_for_output_with_audio(
        input,
        tier,
        hardware_encoder,
        video_bitrate,
        FfmpegTimelineOptions {
            start_time_ticks,
            audio_stream_index: None,
            preserve_input_timestamps: output.preserve_input_timestamps,
        },
        output,
    )
}

fn ffmpeg_args_for_output_with_audio(
    input: &Path,
    tier: ServerTier,
    hardware_encoder: Option<&str>,
    video_bitrate: Option<i64>,
    timing: FfmpegTimelineOptions,
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
        // Progress reports are the only stdout consumer: the media goes to segment files, and
        // the reader task turns these lines into the session's live statistics.
        "-progress".to_owned(),
        "pipe:1".to_owned(),
    ];
    let start_time = ffmpeg_start_time(timing.start_time_ticks);
    if let Some(start_time) = start_time.as_ref() {
        args.extend(["-ss".to_owned(), start_time.clone()]);
    }
    args.extend([
        "-i".to_owned(),
        input.to_string_lossy().into_owned(),
        "-map".to_owned(),
        "0:v:0?".to_owned(),
        "-map".to_owned(),
        timing
            .audio_stream_index
            .map(|index| format!("0:{index}?"))
            .unwrap_or_else(|| "0:a:0?".to_owned()),
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
        // Direct play never transcodes; the caller only builds these arguments for transcode
        // tiers, so emitting no codec arguments is the safe fallback.
        ServerTier::Direct => {}
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
        match output.segment_container {
            HlsSegmentContainer::MpegTs => "mpegts".to_owned(),
            HlsSegmentContainer::FragmentedMp4 => "fmp4".to_owned(),
        },
        "-start_number".to_owned(),
        hls_start_number(timing.start_time_ticks).to_string(),
    ]);
    if output.segment_container.uses_initialization_segment() {
        args.extend([
            "-hls_fmp4_init_filename".to_owned(),
            output.init_file_name.to_owned(),
        ]);
    }
    args.extend([
        "-hls_segment_filename".to_owned(),
        output.segment_file_name,
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
    let segment_number = if let Some(value) = asset.strip_prefix("segment_") {
        value
            .strip_suffix(".m4s")
            .or_else(|| value.strip_suffix(".ts"))
    } else if let Some(value) = asset.strip_prefix("generation_") {
        value.split_once("_segment_").and_then(|(_, value)| {
            value
                .strip_suffix(".m4s")
                .or_else(|| value.strip_suffix(".ts"))
        })
    } else {
        None
    };
    let Some(segment_number) = segment_number else {
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

fn emby_init_segment_number(asset: &str) -> Option<i64> {
    let value = asset
        .strip_prefix("init_")
        .and_then(|value| value.strip_suffix(".mp4"))?;
    if value.len() != 6 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
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

fn vod_manifest(runtime_ticks: i64, segment_container: HlsSegmentContainer) -> Option<String> {
    if runtime_ticks <= 0 {
        return None;
    }
    const TICKS_PER_SECOND: i64 = 10_000_000;
    let full_segments = runtime_ticks / HLS_SEGMENT_DURATION_TICKS;
    let remainder_ticks = runtime_ticks % HLS_SEGMENT_DURATION_TICKS;
    let segment_count = full_segments.checked_add(i64::from(remainder_ticks > 0))?;
    if segment_count > MAX_EMBY_VOD_SEGMENTS {
        return None;
    }
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
    for index in 0..segment_count {
        if segment_container.uses_initialization_segment() {
            let _ = writeln!(manifest, "#EXT-X-MAP:URI=\"init_{index:06}.mp4\"");
        }
        let duration_ticks = if index < full_segments {
            HLS_SEGMENT_DURATION_TICKS
        } else {
            remainder_ticks
        };
        let duration = hls_duration_seconds(duration_ticks);
        let _ = writeln!(manifest, "#EXTINF:{duration},");
        let _ = writeln!(
            manifest,
            "segment_{index:06}.{}",
            segment_container.segment_extension()
        );
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

/// Session identifiers must stay a single portable path segment: they are joined into the HLS
/// root and the resulting directory is later removed recursively.
fn is_safe_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SESSION_ID_BYTES
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

fn is_valid_asset(asset: &str, segment_container: HlsSegmentContainer) -> bool {
    if asset.is_empty() || asset.len() > 128 {
        return false;
    }
    // Every artifact ffmpeg writes for a session sits directly in the session directory, so a
    // single normal component is the whole contract (this also rejects `.`/`..` and any `/`).
    let path = Path::new(asset);
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return false;
    }
    if asset == "index.m3u8" {
        return true;
    }
    if segment_container.uses_initialization_segment()
        && (asset == "init.mp4" || emby_init_segment_number(asset).is_some())
    {
        return true;
    }
    let extension = format!(".{}", segment_container.segment_extension());
    (asset.starts_with("segment_") && asset.ends_with(&extension))
        || is_emby_generation_asset(asset, segment_container)
}

fn is_emby_generation_asset(asset: &str, segment_container: HlsSegmentContainer) -> bool {
    let Some(value) = asset.strip_prefix("generation_") else {
        return false;
    };
    let Some((generation, asset)) = value.split_once('_') else {
        return false;
    };
    let extension = format!(".{}", segment_container.segment_extension());
    let valid_segment = asset
        .strip_prefix("segment_")
        .and_then(|value| value.strip_suffix(&extension))
        .is_some_and(|value| value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_digit()));
    let valid_init = segment_container.uses_initialization_segment() && asset == "init.mp4";
    generation.len() == 6
        && generation.bytes().all(|byte| byte.is_ascii_digit())
        && (valid_init || valid_segment)
}

fn is_allowed_hardware_encoder(value: &str) -> bool {
    matches!(
        value,
        "h264_nvenc" | "h264_vaapi" | "h264_qsv" | "h264_videotoolbox"
    )
}

async fn has_sufficient_free_space(path: &Path, minimum: u64) -> bool {
    let path = path.to_owned();
    // statvfs is a synchronous syscall that can block on a slow NAS mount.
    tokio::task::spawn_blocking(move || {
        available_free_bytes(&path).is_ok_and(|available| available >= minimum)
    })
    .await
    .unwrap_or(false)
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
        path::{Path, PathBuf},
        sync::Arc,
        time::{Duration, Instant},
    };

    use tokio::sync::Mutex;

    use super::{
        HLS_SEGMENT_DURATION_TICKS, HlsError, HlsSegmentContainer, HlsStartOptions,
        SESSION_QUOTA_CACHE_TTL, ServerTier, SessionQuotaCache, asset_segment_number,
        emby_init_segment_number, ffmpeg_args, ffmpeg_args_for_output, ffmpeg_args_with_audio,
        ffmpeg_args_with_timeline, hls_segment_path, hls_start_number, is_valid_asset,
        vod_manifest,
    };

    fn fake_process(directory: PathBuf, total_bytes: Option<u64>) -> super::HlsProcess {
        super::HlsProcess {
            directory: directory.clone(),
            initial_manifest: directory.join("index.m3u8"),
            runtime_ticks: None,
            specification: super::HlsProcessSpecification {
                input: PathBuf::from("/media/movie.mkv"),
                tier: ServerTier::Remux,
                video_bitrate: None,
                audio_stream_index: None,
                emby_vod: false,
                segment_container: HlsSegmentContainer::MpegTs,
            },
            state: Mutex::new(super::HlsProcessState {
                child: None,
                generation: 0,
                emby_segment_generations: std::collections::HashMap::new(),
                init: directory.join("init.mp4"),
                manifest: directory.join("index.m3u8"),
                permit: None,
                segment_start_number: 0,
            }),
            restart: Mutex::new(()),
            quota: Mutex::new(super::SessionQuotaCache {
                checked_at: total_bytes.map(|_| Instant::now()),
                total_bytes: total_bytes.unwrap_or_default(),
            }),
            progress: Arc::new(Mutex::new(super::HlsProgressSnapshot::default())),
        }
    }

    #[tokio::test]
    async fn global_session_budget_accounts_for_every_session_directory() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config_dir = temp_dir.path().join("config");
        let manager =
            super::HlsManager::new_for_tests(config_dir.clone(), "/usr/bin/false".to_owned())
                .with_global_session_budget(1_000);
        assert!(
            manager
                .session_bytes_within_global_budget()
                .await
                .expect("empty budget check"),
            "no session directory means no usage"
        );

        let session_directory = config_dir.join("web-playback/session-a");
        std::fs::create_dir_all(&session_directory).expect("session directory");
        std::fs::write(session_directory.join("segment_000000.ts"), vec![0_u8; 600])
            .expect("segment");
        manager.processes.write().await.insert(
            "session-a".to_owned(),
            Arc::new(fake_process(session_directory.clone(), None)),
        );
        assert!(
            manager
                .session_bytes_within_global_budget()
                .await
                .expect("stale budget check"),
            "the stale cache must fall back to measuring the directory"
        );
        let measured = {
            let processes = manager.processes.read().await;
            let process = processes.get("session-a").expect("process").clone();
            drop(processes);
            let quota = process.quota.lock().await;
            assert!(quota.is_fresh(Instant::now()));
            quota.total_bytes
        };
        assert_eq!(measured, 600);

        let second_directory = config_dir.join("web-playback/session-b");
        std::fs::create_dir_all(&second_directory).expect("session directory");
        std::fs::write(second_directory.join("segment_000000.ts"), vec![0_u8; 600])
            .expect("segment");
        manager.processes.write().await.insert(
            "session-b".to_owned(),
            Arc::new(fake_process(second_directory, Some(600))),
        );
        assert!(
            !manager
                .session_bytes_within_global_budget()
                .await
                .expect("over-budget check"),
            "two sessions above the shared budget must refuse another start"
        );

        manager.processes.write().await.clear();
        manager.processes.write().await.insert(
            "gone".to_owned(),
            Arc::new(fake_process(config_dir.join("web-playback/gone"), None)),
        );
        assert!(
            manager
                .session_bytes_within_global_budget()
                .await
                .expect("missing directory check"),
            "a registered session whose directory vanished must not block new starts"
        );
    }

    #[tokio::test]
    async fn exhausted_global_budget_refuses_to_start_a_session() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config_dir = temp_dir.path().join("config");
        let manager =
            super::HlsManager::new_for_tests(config_dir.clone(), "/usr/bin/false".to_owned())
                .with_global_session_budget(100);
        let session_directory = config_dir.join("web-playback/existing");
        std::fs::create_dir_all(&session_directory).expect("session directory");
        std::fs::write(session_directory.join("segment_000000.ts"), vec![0_u8; 200])
            .expect("segment");
        manager.processes.write().await.insert(
            "existing".to_owned(),
            Arc::new(fake_process(session_directory, Some(200))),
        );

        let error = manager
            .start_emby_vod(
                "new-session",
                ServerTier::Remux,
                Path::new("/media/movie.mkv"),
                None,
                super::HlsStartOptions {
                    segment_container: HlsSegmentContainer::MpegTs,
                    start_time_ticks: None,
                    runtime_ticks: None,
                },
            )
            .await
            .expect_err("the global budget must refuse a new session");
        assert!(matches!(error, HlsError::Limit));
        assert!(
            !manager.processes.read().await.contains_key("new-session"),
            "a refused session must not be registered"
        );
    }

    #[test]
    fn pgrep_patterns_escape_regex_metacharacters() {
        assert_eq!(
            super::escape_pgrep_pattern("/tmp/lux+config/ffmpeg.bin"),
            "/tmp/lux\\+config/ffmpeg\\.bin"
        );
        assert_eq!(super::escape_pgrep_pattern("ffmpeg"), "ffmpeg");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_orphans_terminates_stale_ffmpeg_processes() {
        use std::os::unix::{fs::PermissionsExt, process::ExitStatusExt};

        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config_dir = temp_dir.path().join("config");
        let base_directory = config_dir.join("web-playback");
        let stale_session = base_directory.join("stale-session");
        std::fs::create_dir_all(&stale_session).expect("stale session directory");
        let script = base_directory.join("ffmpeg");
        std::fs::write(&script, "#!/bin/sh\nsleep 60\n").expect("write fake ffmpeg");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .expect("make fake ffmpeg executable");
        let manager = super::HlsManager::new_for_tests(
            config_dir.clone(),
            script.to_string_lossy().into_owned(),
        );
        let mut child = super::Command::new(&script)
            .spawn()
            .expect("spawn stale fake ffmpeg");
        let child_pid = child.id().expect("child pid");
        let pattern = super::escape_pgrep_pattern(&script.to_string_lossy());
        let mut visible = false;
        for _ in 0..100 {
            if super::process_ids_matching(&pattern)
                .await
                .is_some_and(|pids| pids.contains(&child_pid))
            {
                visible = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(visible, "the stale process must be visible to pgrep");

        manager.cleanup_orphans().await.expect("cleanup orphans");

        let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
            .await
            .expect("the orphaned ffmpeg process must be terminated")
            .expect("wait for the orphaned process");
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert!(
            !stale_session.exists(),
            "stale session directories are removed"
        );
    }

    #[test]
    fn tracked_segment_generations_stay_within_their_budget() {
        let mut state = super::HlsProcessState {
            child: None,
            generation: 3,
            emby_segment_generations: std::collections::HashMap::new(),
            init: std::path::PathBuf::new(),
            manifest: std::path::PathBuf::new(),
            permit: None,
            segment_start_number: 0,
        };
        for segment in 0..super::HLS_MAX_TRACKED_SEGMENTS as i64 {
            super::remember_segment_generation(&mut state, segment, 1);
        }
        assert_eq!(
            state.emby_segment_generations.len(),
            super::HLS_MAX_TRACKED_SEGMENTS
        );
        // One more entry resets the map instead of growing past the bound.
        super::remember_segment_generation(&mut state, 9_999, 2);
        assert_eq!(state.emby_segment_generations.len(), 1);
        assert_eq!(state.emby_segment_generations.get(&9_999), Some(&2));
    }

    #[test]
    fn hls_assets_are_single_file_names() {
        let container = HlsSegmentContainer::FragmentedMp4;
        assert!(is_valid_asset("index.m3u8", container));
        assert!(is_valid_asset("init_000001.mp4", container));
        assert!(is_valid_asset("segment_000001.m4s", container));
        for rejected in [
            "nested/segment_000001.m4s",
            "../segment_000001.m4s",
            "./segment_000001.m4s",
            "/tmp/segment_000001.m4s",
            "..",
            "",
        ] {
            assert!(
                !is_valid_asset(rejected, container),
                "{rejected} must be rejected"
            );
        }
    }

    #[test]
    fn vod_manifest_declares_the_complete_runtime() {
        let manifest = vod_manifest(
            9 * 10_000_000 + 5_000_000,
            HlsSegmentContainer::FragmentedMp4,
        )
        .expect("positive runtime");

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
    fn vod_manifest_binds_each_segment_to_its_initialization_segment() {
        let manifest = vod_manifest(
            9 * 10_000_000 + 5_000_000,
            HlsSegmentContainer::FragmentedMp4,
        )
        .expect("positive runtime");

        assert_eq!(manifest.matches("#EXT-X-MAP:").count(), 3);
        assert!(manifest.contains("#EXT-X-MAP:URI=\"init_000000.mp4\"\n"));
        assert!(manifest.contains("#EXT-X-MAP:URI=\"init_000001.mp4\"\n"));
        assert!(manifest.contains("#EXT-X-MAP:URI=\"init_000002.mp4\"\n"));
        assert!(manifest.contains(
            "#EXT-X-MAP:URI=\"init_000001.mp4\"\n#EXTINF:4.000000,\nsegment_000001.m4s\n"
        ));
    }

    #[test]
    fn ts_vod_manifest_has_independent_ts_segments_without_an_init_map() {
        let manifest = vod_manifest(9 * 10_000_000 + 5_000_000, HlsSegmentContainer::MpegTs)
            .expect("positive runtime");

        assert!(!manifest.contains("#EXT-X-MAP"));
        assert!(manifest.contains("segment_000000.ts\n"));
        assert!(manifest.contains("segment_000002.ts\n"));
        assert!(manifest.contains("#EXT-X-ENDLIST\n"));
    }

    #[test]
    fn hls_container_contract_exposes_matching_extensions_and_mime_types() {
        assert_eq!(HlsSegmentContainer::MpegTs.segment_extension(), "ts");
        assert_eq!(HlsSegmentContainer::MpegTs.mime_type(), "video/mp2t");
        assert_eq!(
            HlsSegmentContainer::FragmentedMp4.segment_extension(),
            "m4s"
        );
        assert_eq!(HlsSegmentContainer::FragmentedMp4.mime_type(), "video/mp4");
    }

    #[test]
    fn emby_ts_ffmpeg_arguments_do_not_request_fmp4_or_an_init_file() {
        let args = ffmpeg_args_for_output(
            Path::new("movie.mkv"),
            Path::new("session"),
            ServerTier::SoftwareTranscode,
            None,
            None,
            None,
            super::HlsOutput {
                init_file_name: "init.mp4",
                manifest: Path::new("session/index.m3u8"),
                segment_file_name: "session/segment_%06d.ts".to_owned(),
                segment_container: HlsSegmentContainer::MpegTs,
                preserve_input_timestamps: true,
            },
        )
        .unwrap();

        assert!(
            args.windows(2)
                .any(|pair| pair == ["-hls_segment_type", "mpegts"])
        );
        assert!(!args.iter().any(|value| value == "-hls_fmp4_init_filename"));
        assert!(args.iter().any(|value| value.ends_with("segment_%06d.ts")));
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
        assert!(args.windows(2).any(|pair| pair == ["-progress", "pipe:1"]));
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
    fn selected_audio_stream_maps_the_source_stream_index() {
        let args = ffmpeg_args_with_audio(
            Path::new("movie.mkv"),
            Path::new("session"),
            ServerTier::Remux,
            None,
            None,
            None,
            Some(3),
        )
        .unwrap();

        assert!(args.windows(2).any(|pair| pair == ["-map", "0:3?"]));
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
        assert_eq!(
            asset_segment_number("generation_000001_segment_000969.m4s").unwrap(),
            Some(969)
        );
        assert_eq!(asset_segment_number("init.mp4").unwrap(), None);
        assert!(asset_segment_number("segment_invalid.m4s").is_err());
    }

    #[test]
    fn initialization_assets_keep_their_logical_number() {
        assert_eq!(emby_init_segment_number("init_000000.mp4"), Some(0));
        assert_eq!(emby_init_segment_number("init_000969.mp4"), Some(969));
        assert_eq!(emby_init_segment_number("init.mp4"), None);
        assert_eq!(emby_init_segment_number("generation_000001_init.mp4"), None);
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
        let fmp4 = HlsSegmentContainer::FragmentedMp4;
        assert!(is_valid_asset("index.m3u8", fmp4));
        assert!(is_valid_asset("init_000001.mp4", fmp4));
        assert!(is_valid_asset("segment_000001.m4s", fmp4));
        assert!(is_valid_asset("generation_000001_init.mp4", fmp4));
        assert!(is_valid_asset("generation_000001_segment_000001.m4s", fmp4));
        assert!(!is_valid_asset("generation_000001_segment_bad.m4s", fmp4));
        assert!(!is_valid_asset("../index.m3u8", fmp4));
        assert!(!is_valid_asset("other.txt", fmp4));
        assert!(is_valid_asset(
            "segment_000001.ts",
            HlsSegmentContainer::MpegTs
        ));
        assert!(is_valid_asset(
            "generation_000001_segment_000001.ts",
            HlsSegmentContainer::MpegTs
        ));
        assert!(!is_valid_asset(
            "generation_000001_init.mp4",
            HlsSegmentContainer::MpegTs
        ));
        assert!(!is_valid_asset(
            "init_000001.mp4",
            HlsSegmentContainer::MpegTs
        ));
        assert!(!is_valid_asset(
            "segment_000001.m4s",
            HlsSegmentContainer::MpegTs
        ));
    }

    #[test]
    fn emby_generation_segment_paths_are_unique() {
        assert_eq!(
            hls_segment_path(
                Path::new("/config/session"),
                true,
                1,
                969,
                HlsSegmentContainer::FragmentedMp4,
            ),
            Path::new("/config/session/generation_000001_segment_000969.m4s")
        );
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

    #[tokio::test]
    async fn manager_refuses_session_ids_that_escape_the_session_root() {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_dir = temp_dir.path().join("config");
        let base_directory = config_dir.join("web-playback");
        tokio::fs::create_dir_all(&base_directory).await.unwrap();
        let sentinel = config_dir.join("lux.db");
        tokio::fs::write(&sentinel, b"database").await.unwrap();
        let manager = super::HlsManager::new_for_tests(config_dir.clone(), "ffmpeg".to_owned());

        for session_id in ["", ".", "..", "../..", "a/b", "a\\b", "../outside"] {
            let result = manager.stop(session_id).await;
            assert!(
                matches!(result, Err(super::HlsError::NotFound)),
                "session id {session_id:?} must be rejected"
            );
            assert!(
                sentinel.exists(),
                "session id {session_id:?} must not delete files outside the session root"
            );
        }
        assert!(base_directory.exists());
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
    async fn emby_first_segment_starts_initial_generation_and_backward_seek_restarts() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::tempdir().unwrap();
        let script = temp_dir.path().join("fake-ffmpeg-generations");
        let script_body = r##"#!/bin/sh
set -eu
manifest=""
segment=""
init="init.mp4"
start_number=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    -hls_segment_filename) segment="$2"; shift 2 ;;
    -hls_fmp4_init_filename) init="$2"; shift 2 ;;
    -start_number) start_number="$2"; shift 2 ;;
    *.m3u8) manifest="$1"; shift ;;
    *) shift ;;
  esac
done
directory=$(dirname "$manifest")
mkdir -p "$directory"
printf '%s\n' "$start_number" >> "$directory/starts.log"
printf '#EXTM3U\n#EXT-X-MAP:URI="%s"\n#EXTINF:4,\nsegment_%06d.m4s\n' "$init" "$start_number" > "$manifest"
printf init > "$directory/$init"
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
                HlsStartOptions {
                    segment_container: HlsSegmentContainer::FragmentedMp4,
                    start_time_ticks: Some(100 * HLS_SEGMENT_DURATION_TICKS),
                    runtime_ticks: Some(600 * 10_000_000),
                },
            )
            .await
            .unwrap();
        let session_directory = manager.session_directory("emby-generations").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !session_directory.join("starts.log").exists(),
            "Emby VOD must not start FFmpeg before the first asset request"
        );

        let forward = manager
            .wait_for_asset("emby-generations", "segment_000100.m4s")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(forward).await.unwrap(), b"segment-100");
        let initial = manager
            .wait_for_asset("emby-generations", "init_000100.mp4")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(initial).await.unwrap(), b"init");
        let initial_manifest = session_directory.join("generation_000000.m3u8");
        assert_eq!(
            initial_manifest.file_name().and_then(|name| name.to_str()),
            Some("generation_000000.m3u8")
        );
        assert!(initial_manifest.exists());

        let backward = manager
            .wait_for_asset("emby-generations", "segment_000050.m4s")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(backward).await.unwrap(), b"segment-50");
        let original_init = manager
            .wait_for_asset("emby-generations", "init_000100.mp4")
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(original_init).await.unwrap(), b"init");
        let original_segment = manager
            .wait_for_asset("emby-generations", "segment_000100.m4s")
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(original_segment).await.unwrap(),
            b"segment-100"
        );
        assert!(initial_manifest.exists());

        assert!(session_directory.join("generation_000001.m3u8").exists());
        assert!(!session_directory.join("generation_000002.m3u8").exists());
        assert_eq!(
            tokio::fs::read_to_string(session_directory.join("starts.log"))
                .await
                .unwrap(),
            "100\n50\n"
        );

        manager.stop("emby-generations").await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn emby_init_and_segment_share_the_requested_generation() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::tempdir().unwrap();
        let script = temp_dir.path().join("fake-ffmpeg-init-race");
        let script_body = r##"#!/bin/sh
set -eu
manifest=""
segment=""
init="init.mp4"
start_number=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    -hls_segment_filename) segment="$2"; shift 2 ;;
    -hls_fmp4_init_filename) init="$2"; shift 2 ;;
    -start_number) start_number="$2"; shift 2 ;;
    *.m3u8) manifest="$1"; shift ;;
    *) shift ;;
  esac
done
directory=$(dirname "$manifest")
mkdir -p "$directory"
printf '%s\n' "$start_number" >> "$directory/starts.log"
if [ "$start_number" -eq 100 ]; then
  sleep 1
fi
printf '#EXTM3U\n#EXT-X-MAP:URI="%s"\n#EXTINF:4,\nsegment_%06d.m4s\n' "$init" "$start_number" > "$manifest"
printf 'init-%s' "$start_number" > "$directory/$init"
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
                "emby-init-race",
                ServerTier::SoftwareTranscode,
                Path::new("input.mkv"),
                Some(1_000_000),
                HlsStartOptions {
                    segment_container: HlsSegmentContainer::FragmentedMp4,
                    start_time_ticks: Some(100 * HLS_SEGMENT_DURATION_TICKS),
                    runtime_ticks: Some(600 * 10_000_000),
                },
            )
            .await
            .unwrap();

        let init_manager = manager.clone();
        let init_request = tokio::spawn(async move {
            init_manager
                .wait_for_asset("emby-init-race", "init_000100.mp4")
                .await
        });
        let session_directory = manager.session_directory("emby-init-race").await.unwrap();
        for _ in 0..250 {
            if tokio::fs::read_to_string(session_directory.join("starts.log"))
                .await
                .is_ok_and(|starts| starts == "100\n")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            tokio::fs::read_to_string(session_directory.join("starts.log"))
                .await
                .unwrap(),
            "100\n"
        );

        let segment_manager = manager.clone();
        let segment_request = tokio::spawn(async move {
            segment_manager
                .wait_for_asset("emby-init-race", "segment_000050.m4s")
                .await
        });
        let init = init_request.await.unwrap().unwrap();
        assert_eq!(tokio::fs::read(init).await.unwrap(), b"init-100");
        let segment = segment_request.await.unwrap().unwrap();
        assert_eq!(tokio::fs::read(segment).await.unwrap(), b"segment-50");
        assert_eq!(
            tokio::fs::read_to_string(session_directory.join("starts.log"))
                .await
                .unwrap(),
            "100\n50\n"
        );

        manager.stop("emby-init-race").await.unwrap();
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
init="init.mp4"
start_number=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    -hls_segment_filename) segment="$2"; shift 2 ;;
    -hls_fmp4_init_filename) init="$2"; shift 2 ;;
    -start_number) start_number="$2"; shift 2 ;;
    *.m3u8) manifest="$1"; shift ;;
    *) shift ;;
  esac
done
directory=$(dirname "$manifest")
mkdir -p "$directory"
printf '#EXTM3U\n#EXT-X-MAP:URI="%s"\n#EXTINF:4,\nsegment_%06d.m4s\n' "$init" "$start_number" > "$manifest"
printf init > "$directory/$init"
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
                HlsStartOptions {
                    segment_container: HlsSegmentContainer::FragmentedMp4,
                    start_time_ticks: None,
                    runtime_ticks: Some(1_000 * 10_000_000),
                },
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
init="init.mp4"
start_number=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    -hls_segment_filename) segment="$2"; shift 2 ;;
    -hls_fmp4_init_filename) init="$2"; shift 2 ;;
    -start_number) start_number="$2"; shift 2 ;;
    *.m3u8) manifest="$1"; shift ;;
    *) shift ;;
  esac
done
directory=$(dirname "$manifest")
mkdir -p "$directory"
printf '#EXTM3U\n#EXT-X-MAP:URI="%s"\n#EXTINF:4,\nsegment_%06d.m4s\n' "$init" "$start_number" > "$manifest"
printf init > "$directory/$init"
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
                HlsStartOptions {
                    segment_container: HlsSegmentContainer::FragmentedMp4,
                    start_time_ticks: None,
                    runtime_ticks: Some(600 * 10_000_000),
                },
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

    #[test]
    fn ffmpeg_progress_lines_populate_the_snapshot() {
        let mut snapshot = super::HlsProgressSnapshot {
            generation: 3,
            ..super::HlsProgressSnapshot::default()
        };
        for line in [
            "frame=120",
            "fps=24.00",
            "bitrate=1234.5kbits/s",
            "total_size=987654",
            "out_time_us=5000000",
            "speed=1.53x",
            "dup_frames=1",
            "drop_frames=2",
            "progress=continue",
        ] {
            assert!(snapshot.apply_line(line, 42), "line must be recognized");
        }
        assert_eq!(snapshot.generation, 3);
        assert_eq!(snapshot.frame, Some(120));
        assert_eq!(snapshot.fps, Some(24.0));
        assert_eq!(snapshot.bitrate_kbps, Some(1234.5));
        assert_eq!(snapshot.total_size, Some(987_654));
        assert_eq!(snapshot.out_time_us, Some(5_000_000));
        assert_eq!(snapshot.speed, Some(1.53));
        assert_eq!(snapshot.duplicated_frames, Some(1));
        assert_eq!(snapshot.dropped_frames, Some(2));
        assert!(!snapshot.finished);
        assert_eq!(snapshot.updated_at_ms, 42);

        assert!(snapshot.apply_line("progress=end", 43));
        assert!(snapshot.finished);
        assert_eq!(snapshot.updated_at_ms, 43);
    }

    #[test]
    fn ffmpeg_progress_ignores_unusable_values() {
        let mut snapshot = super::HlsProgressSnapshot::default();
        for line in [
            "bitrate=N/A",
            "speed=N/A",
            "fps=   ",
            "drop_frames=-1",
            "out_time_ms=5000000000",
            "stream_0_0_q=-1.0",
            "no-equals-sign",
            "",
        ] {
            assert!(!snapshot.apply_line(line, 7), "line must be ignored");
        }
        assert_eq!(snapshot, super::HlsProgressSnapshot::default());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ffmpeg_progress_reaches_the_session_snapshot() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::tempdir().unwrap();
        let script = temp_dir.path().join("fake-ffmpeg");
        let source = [
            "#!/bin/sh",
            "printf 'frame=240\\nfps=24.00\\nbitrate=2048.5kbits/s\\ntotal_size=1048576\\n'",
            "printf 'out_time_us=10000000\\nspeed=2.00x\\ndup_frames=0\\ndrop_frames=3\\n'",
            "printf 'progress=end\\n'",
            "sleep 0.3",
        ]
        .join("\n");
        std::fs::write(&script, source).unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions).unwrap();
        let manager = super::HlsManager::new_with_limits(
            temp_dir.path().join("config"),
            script.to_string_lossy().into_owned(),
            0,
        );

        manager
            .start(
                "progress",
                ServerTier::Remux,
                Path::new("input.mkv"),
                None,
                None,
                None,
            )
            .await
            .unwrap();

        let mut snapshot = None;
        for _ in 0..200 {
            snapshot = manager.progress("progress").await;
            if snapshot.as_ref().is_some_and(|value| value.finished) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let snapshot = snapshot.expect("progress must be captured");
        assert_eq!(snapshot.fps, Some(24.0));
        assert_eq!(snapshot.bitrate_kbps, Some(2048.5));
        assert_eq!(snapshot.total_size, Some(1_048_576));
        assert_eq!(snapshot.out_time_us, Some(10_000_000));
        assert_eq!(snapshot.speed, Some(2.0));
        assert_eq!(snapshot.dropped_frames, Some(3));
        assert!(snapshot.finished);
        assert_eq!(snapshot.generation, 0);
        assert!(manager.progress("missing-session").await.is_none());
        let _ = manager.stop("progress").await;
    }
}
