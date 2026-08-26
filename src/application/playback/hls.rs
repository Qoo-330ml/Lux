use std::{
    collections::HashMap,
    fmt,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
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

#[derive(Debug)]
pub(crate) enum HlsError {
    Io(std::io::Error),
    Spawn(String),
    Limit,
    NotFound,
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
            Self::Failed => formatter.write_str("HLS process failed before producing a manifest"),
            Self::InvalidAsset => formatter.write_str("invalid HLS asset"),
        }
    }
}

impl std::error::Error for HlsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Spawn(_) | Self::Limit | Self::NotFound | Self::Failed | Self::InvalidAsset => {
                None
            }
        }
    }
}

struct HlsProcess {
    directory: PathBuf,
    child: Mutex<Option<Child>>,
    _permit: OwnedSemaphorePermit,
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

    pub(crate) async fn start(
        &self,
        session_id: &str,
        tier: ServerTier,
        input: &Path,
    ) -> Result<(), HlsError> {
        let permit = self.acquire_permit(tier).await?;
        fs::create_dir_all(&self.base_directory)
            .await
            .map_err(HlsError::Io)?;
        if !has_sufficient_free_space(&self.base_directory, self.min_free_bytes) {
            return Err(HlsError::Limit);
        }
        let directory = self.base_directory.join(session_id);
        fs::create_dir_all(&directory).await.map_err(HlsError::Io)?;
        let args = ffmpeg_args(input, &directory, tier, self.hardware_encoder.as_deref())?;
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
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_dir_all(&directory).await;
                return Err(HlsError::Spawn(format!(
                    "failed to start HLS process: {error}"
                )));
            }
        };
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
        let process = Arc::new(HlsProcess {
            directory,
            child: Mutex::new(Some(child)),
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

    pub(crate) async fn wait_for_manifest(&self, session_id: &str) -> Result<PathBuf, HlsError> {
        let process = self
            .processes
            .lock()
            .await
            .get(session_id)
            .cloned()
            .ok_or(HlsError::NotFound)?;
        let manifest = process.directory.join("index.m3u8");
        for _ in 0..MANIFEST_WAIT_ATTEMPTS {
            if fs::metadata(&manifest).await.is_ok() {
                return Ok(manifest);
            }
            let finished = {
                let mut child = process.child.lock().await;
                match child.as_mut() {
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
        let path = process.directory.join(asset);
        if !path.starts_with(&process.directory) {
            return Err(HlsError::InvalidAsset);
        }
        Ok(path)
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
        let mut entries = fs::read_dir(&process.directory)
            .await
            .map_err(HlsError::Io)?;
        let mut total = 0_u64;
        while let Some(entry) = entries.next_entry().await.map_err(HlsError::Io)? {
            let metadata = entry.metadata().await.map_err(HlsError::Io)?;
            if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
            if total > MAX_SESSION_BYTES {
                return Ok(false);
            }
        }
        Ok(true)
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

async fn stop_process(process: Arc<HlsProcess>) {
    let mut child = process.child.lock().await;
    if let Some(mut child) = child.take() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    drop(child);
    let _ = fs::remove_dir_all(&process.directory).await;
}

fn ffmpeg_executable() -> String {
    std::env::var("LUX_FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".to_owned())
}

fn ffmpeg_args(
    input: &Path,
    directory: &Path,
    tier: ServerTier,
    hardware_encoder: Option<&str>,
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
        "-i".to_owned(),
        input.to_string_lossy().into_owned(),
        "-map".to_owned(),
        "0:v:0?".to_owned(),
        "-map".to_owned(),
        "0:a:0?".to_owned(),
    ];
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
    args.extend([
        "-f".to_owned(),
        "hls".to_owned(),
        "-hls_time".to_owned(),
        "4".to_owned(),
        "-hls_list_size".to_owned(),
        "0".to_owned(),
        "-hls_segment_type".to_owned(),
        "fmp4".to_owned(),
        "-hls_fmp4_init_filename".to_owned(),
        "init.mp4".to_owned(),
        "-hls_segment_filename".to_owned(),
        directory
            .join("segment_%06d.m4s")
            .to_string_lossy()
            .into_owned(),
        "-hls_flags".to_owned(),
        "independent_segments+temp_file".to_owned(),
        directory.join("index.m3u8").to_string_lossy().into_owned(),
    ]);
    Ok(args)
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
        .checked_mul(statistics.f_frsize as u64)
        .ok_or_else(|| std::io::Error::other("free-space value overflowed"))
}

#[cfg(not(unix))]
fn available_free_bytes(_path: &Path) -> Result<u64, std::io::Error> {
    Ok(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{ServerTier, ffmpeg_args, is_valid_asset};

    #[test]
    fn remux_arguments_copy_video_and_audio_into_cmaf_hls() {
        let args = ffmpeg_args(
            Path::new("/media/movie.mkv"),
            Path::new("/config/web-playback/session"),
            ServerTier::Remux,
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
        )
        .unwrap();
        assert!(args.windows(2).any(|pair| pair == ["-c:v", "libx264"]));
        assert!(args.windows(2).any(|pair| pair == ["-c:a", "aac"]));
    }

    #[test]
    fn asset_validation_rejects_path_traversal_and_unknown_files() {
        assert!(is_valid_asset("index.m3u8"));
        assert!(is_valid_asset("segment_000001.m4s"));
        assert!(!is_valid_asset("../index.m3u8"));
        assert!(!is_valid_asset("other.txt"));
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
            .start("session-1", ServerTier::Remux, Path::new("input.mkv"))
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
    async fn manager_rejects_new_sessions_below_the_free_space_watermark() {
        let temp_dir = tempfile::tempdir().unwrap();
        let manager = super::HlsManager::new_with_limits(
            temp_dir.path().join("config"),
            "/bin/true".to_owned(),
            u64::MAX,
        );

        let error = manager
            .start("low-space", ServerTier::Remux, Path::new("input.mkv"))
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
