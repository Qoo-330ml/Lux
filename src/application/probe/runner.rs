use std::{
    fmt,
    future::Future,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    sync::oneshot,
    time::timeout,
};

use crate::application::background_work::{
    BackgroundWorkPermit, BackgroundWorkStage, acquire_process_background_work,
    process_background_child_cleanup_pending,
};

use super::concurrency::{AttemptOutcome, ProbePermit};
use super::{MAX_OUTPUT_BYTES, MediaProbeResult, parse_probe_json};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_ERROR_BYTES: usize = 8 * 1024;

#[derive(Clone, Debug)]
pub struct FfprobeRunner {
    binary: PathBuf,
    timeout: Duration,
}

impl FfprobeRunner {
    pub fn new(binary: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            binary: binary.into(),
            timeout,
        }
    }

    pub fn default_binary() -> Self {
        Self::new("ffprobe", DEFAULT_TIMEOUT)
    }

    pub async fn probe_path(&self, path: &Path) -> Result<MediaProbeResult, ProbeError> {
        self.probe_path_inner(path, None).await
    }

    pub(super) async fn probe_path_with_permit(
        &self,
        path: &Path,
        permit: ProbePermit,
    ) -> Result<MediaProbeResult, ProbeError> {
        self.probe_path_inner(path, Some(permit)).await
    }

    async fn probe_path_inner(
        &self,
        path: &Path,
        permit: Option<ProbePermit>,
    ) -> Result<MediaProbeResult, ProbeError> {
        let runner = self.clone();
        let path = path.to_owned();
        let (sender, receiver) = oneshot::channel();
        drop(tokio::spawn(async move {
            let mut sender = sender;
            let result = runner.run_path(&path, &mut sender, permit).await;
            let _ = sender.send(result);
        }));
        receiver.await.map_err(|_| {
            ProbeError::Io(std::io::Error::other(
                "ffprobe process owner stopped before returning a result",
            ))
        })?
    }

    async fn run_path(
        &self,
        path: &Path,
        response: &mut oneshot::Sender<Result<MediaProbeResult, ProbeError>>,
        mut permit: Option<ProbePermit>,
    ) -> Result<MediaProbeResult, ProbeError> {
        let background_permit = tokio::select! {
            biased;
            _ = response.closed() => {
                if let Some(permit) = permit.take() {
                    permit.complete(AttemptOutcome::Cancelled);
                }
                return Err(ProbeError::Io(std::io::Error::other(
                    "ffprobe process canceled before admission",
                )));
            }
            result = acquire_process_background_work(BackgroundWorkStage::Probe) => match result {
                Ok(permit) => permit,
                Err(error) => {
                    if let Some(permit) = permit.take() {
                        permit.complete(AttemptOutcome::Failure);
                    }
                    return Err(ProbeError::Io(std::io::Error::other(error.to_string())));
                }
            }
        };
        let child = Command::new(&self.binary)
            .args([
                "-v",
                "error",
                "-print_format",
                "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let child = match child {
            Ok(child) => child,
            Err(error) => {
                if let Some(permit) = permit {
                    permit.complete(AttemptOutcome::Failure);
                }
                drop(background_permit);
                return Err(ProbeError::Io(error));
            }
        };
        let mut owner = ProcessOwner::new(child, permit, background_permit);
        let child = owner.child.as_mut().ok_or_else(|| {
            ProbeError::Io(std::io::Error::other("ffprobe child ownership was lost"))
        })?;
        let output = tokio::select! {
            _ = response.closed() => None,
            output = timeout(self.timeout, collect_process_output(child)) => Some(output),
        };
        let output = match output {
            None => {
                owner.transfer_to_reaper(AttemptOutcome::Cancelled);
                return Err(ProbeError::Io(std::io::Error::other(
                    "ffprobe process canceled before returning a result",
                )));
            }
            Some(Ok(Ok(output))) => {
                owner.mark_reaped();
                output
            }
            Some(Ok(Err(error))) => {
                let outcome = if matches!(error, ProbeError::Timeout) {
                    AttemptOutcome::Timeout
                } else {
                    AttemptOutcome::Failure
                };
                owner.transfer_to_reaper(outcome);
                return Err(error);
            }
            Some(Err(_)) => {
                owner.transfer_to_reaper(AttemptOutcome::Timeout);
                return Err(ProbeError::Timeout);
            }
        };
        if !output.status.success() {
            owner.complete(AttemptOutcome::Failure);
            return Err(ProbeError::Exit {
                code: output.status.code(),
                stderr: truncate(&output.stderr),
            });
        }
        let parsed = parse_probe_json(&output.stdout);
        owner.complete(if parsed.is_ok() {
            AttemptOutcome::Success
        } else {
            AttemptOutcome::Failure
        });
        parsed
    }
}

impl Default for FfprobeRunner {
    fn default() -> Self {
        Self::default_binary()
    }
}

#[derive(Debug)]
pub enum ProbeError {
    Io(std::io::Error),
    Timeout,
    Exit { code: Option<i32>, stderr: String },
    OutputTooLarge,
    InvalidOutput(String),
}

impl ProbeError {
    pub fn failure_status(&self) -> &'static str {
        match self {
            Self::Timeout => "TIMEOUT",
            Self::Exit { .. } => "FAILED",
            Self::Io(_) => "FAILED",
            Self::OutputTooLarge | Self::InvalidOutput(_) => "FAILED",
        }
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "ffprobe process failed: {error}"),
            Self::Timeout => formatter.write_str("ffprobe timed out"),
            Self::Exit { code, stderr } => {
                write!(formatter, "ffprobe exited with {:?}: {}", code, stderr)
            }
            Self::OutputTooLarge => formatter.write_str("ffprobe output exceeds size limit"),
            Self::InvalidOutput(error) => write!(formatter, "invalid ffprobe output: {error}"),
        }
    }
}

impl std::error::Error for ProbeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Timeout | Self::Exit { .. } | Self::OutputTooLarge | Self::InvalidOutput(_) => {
                None
            }
        }
    }
}

fn truncate(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_ERROR_BYTES)]);
    text.trim().to_owned()
}

pub(super) async fn read_limited<R>(mut reader: R, max_bytes: usize) -> Result<Vec<u8>, ProbeError>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(max_bytes.min(8 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let count = reader.read(&mut buffer).await.map_err(ProbeError::Io)?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len().saturating_add(count) > max_bytes {
            return Err(ProbeError::OutputTooLarge);
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
}

struct CollectedProcessOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn collect_process_output(child: &mut Child) -> Result<CollectedProcessOutput, ProbeError> {
    let stdout = child.stdout.take().ok_or_else(|| {
        ProbeError::Io(std::io::Error::other("ffprobe stdout pipe is unavailable"))
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ProbeError::Io(std::io::Error::other("ffprobe stderr pipe is unavailable"))
    })?;
    let mut stdout_future = Box::pin(read_limited(stdout, MAX_OUTPUT_BYTES));
    let mut stderr_future = Box::pin(read_limited(stderr, MAX_ERROR_BYTES));
    let mut stdout_result = None;
    let mut stderr_result = None;
    while stdout_result.is_none() || stderr_result.is_none() {
        tokio::select! {
            result = &mut stdout_future, if stdout_result.is_none() => {
                match result {
                    Ok(bytes) => stdout_result = Some(Ok(bytes)),
                    Err(error) => return Err(error),
                }
            }
            result = &mut stderr_future, if stderr_result.is_none() => {
                match result {
                    Ok(bytes) => stderr_result = Some(Ok(bytes)),
                    Err(error) => return Err(error),
                }
            }
        }
    }
    let stdout = stdout_result.ok_or_else(|| {
        ProbeError::Io(std::io::Error::other("ffprobe stdout read did not finish"))
    })??;
    let stderr = stderr_result.ok_or_else(|| {
        ProbeError::Io(std::io::Error::other("ffprobe stderr read did not finish"))
    })??;
    let status = child.wait().await.map_err(ProbeError::Io)?;
    Ok(CollectedProcessOutput {
        status,
        stdout,
        stderr,
    })
}

struct ProcessOwner {
    child: Option<Child>,
    permit: Option<ProbePermit>,
    background_permit: Option<BackgroundWorkPermit>,
}

impl ProcessOwner {
    fn new(
        child: Child,
        permit: Option<ProbePermit>,
        background_permit: BackgroundWorkPermit,
    ) -> Self {
        Self {
            child: Some(child),
            permit,
            background_permit: Some(background_permit),
        }
    }

    fn mark_reaped(&mut self) {
        self.child.take();
    }

    fn complete(&mut self, outcome: AttemptOutcome) {
        if let Some(permit) = self.permit.take() {
            permit.complete(outcome);
        }
        drop(self.background_permit.take());
    }

    fn transfer_to_reaper(&mut self, outcome: AttemptOutcome) {
        let Some(child) = self.child.take() else {
            self.complete(outcome);
            return;
        };
        if outcome == AttemptOutcome::Cancelled
            && let Some(permit) = self.background_permit.as_mut()
        {
            permit.mark_cancelled_running();
        }
        let permit = self.permit.take();
        let background_permit = self.background_permit.take();
        spawn_reaper(child, permit, background_permit, outcome, |child| {
            Box::pin(child.wait())
        });
    }
}

impl Drop for ProcessOwner {
    fn drop(&mut self) {
        self.transfer_to_reaper(AttemptOutcome::Cancelled);
    }
}

type ChildWaitFuture<'a> =
    std::pin::Pin<Box<dyn Future<Output = std::io::Result<std::process::ExitStatus>> + Send + 'a>>;

fn spawn_reaper<F>(
    mut child: Child,
    permit: Option<ProbePermit>,
    background_permit: Option<BackgroundWorkPermit>,
    outcome: AttemptOutcome,
    mut wait: F,
) where
    F: for<'a> FnMut(&'a mut Child) -> ChildWaitFuture<'a> + Send + 'static,
{
    let _ = child.start_kill();
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                let cleanup_pending =
                    process_background_child_cleanup_pending(BackgroundWorkStage::Probe).await;
                terminate_child_with_wait(&mut child, |child| wait(child)).await;
                if let Some(permit) = permit {
                    permit.complete(outcome);
                }
                drop(background_permit);
                drop(cleanup_pending);
            });
        }
        Err(error) => {
            tracing::error!(%error, pid = child.id(), "ffprobe runtime ended before child reaping");
            drop(child);
            drop(permit);
            drop(background_permit);
        }
    }
}

async fn terminate_child_with_wait<F>(child: &mut Child, mut wait: F)
where
    F: for<'a> FnMut(&'a mut Child) -> ChildWaitFuture<'a>,
{
    let _ = child.start_kill();
    let mut next_warning = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let result = wait(child).await;
        if result.is_ok() {
            return;
        }
        if tokio::time::Instant::now() >= next_warning {
            tracing::warn!(
                pid = child.id(),
                "ffprobe child exit could not be confirmed; retaining process ownership"
            );
            next_warning = tokio::time::Instant::now() + Duration::from_secs(30);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    use tokio::{
        process::Command,
        sync::oneshot,
        time::{Duration, sleep, timeout},
    };

    use crate::{application::probe::concurrency::ProbeRuntime, domain::ids::LibraryId};

    use super::{AttemptOutcome, ChildWaitFuture, FfprobeRunner, spawn_reaper};

    #[tokio::test]
    async fn process_permit_stays_held_until_wait_confirms_child_exit() {
        let child = Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .expect("sleep process should start");
        let now = std::time::Instant::now();
        let runtime = ProbeRuntime::new(1, 1, now);
        let session = runtime
            .register(LibraryId::new(), 1, 1, now)
            .expect("session should register");
        let permit = session
            .try_acquire(now)
            .expect("admission state")
            .expect("permit should be granted");
        let (wait_started_tx, wait_started_rx) = oneshot::channel();
        let (release_wait_tx, release_wait_rx) = oneshot::channel();
        let mut wait_started_tx = Some(wait_started_tx);
        let mut release_wait_rx = Some(release_wait_rx);
        spawn_reaper(
            child,
            Some(permit),
            None,
            AttemptOutcome::Cancelled,
            move |child| {
                let wait_started_tx = wait_started_tx.take();
                let release_wait_rx = release_wait_rx.take();
                Box::pin(async move {
                    if let Some(wait_started_tx) = wait_started_tx {
                        let _ = wait_started_tx.send(());
                    }
                    let gate_result = if let Some(release_wait_rx) = release_wait_rx {
                        release_wait_rx.await.map_err(|_| {
                            std::io::Error::other("test wait gate closed before child reaping")
                        })
                    } else {
                        Ok(())
                    };
                    match gate_result {
                        Ok(()) => child.wait().await,
                        Err(error) => Err(error),
                    }
                }) as ChildWaitFuture<'_>
            },
        );

        wait_started_rx
            .await
            .expect("cleanup should reach child wait");
        sleep(Duration::from_millis(1_100)).await;
        assert_eq!(runtime.active_count().expect("active count"), 1);

        let _ = release_wait_tx.send(());
        timeout(Duration::from_secs(5), async {
            while runtime.active_count().expect("active count") != 0 {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("permit should release after wait confirms exit");
    }

    #[tokio::test]
    async fn cancelling_probe_retains_its_runtime_permit_until_child_is_reaped() {
        let directory = tempfile::tempdir().expect("temporary directory should be created");
        let binary = directory.path().join("fake-ffprobe");
        let marker = directory.path().join("child.pid");
        let script = format!(
            "#!/bin/sh\necho $$ > {}\nexec sleep 30\n",
            shell_quote(&marker.to_string_lossy())
        );
        fs::write(&binary, script).expect("fake ffprobe should be written");
        let mut permissions = fs::metadata(&binary)
            .expect("fake ffprobe metadata should be readable")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions).expect("fake ffprobe should be executable");

        let now = std::time::Instant::now();
        let runtime = ProbeRuntime::new(1, 1, now);
        let session = runtime
            .register(LibraryId::new(), 1, 1, now)
            .expect("probe session should register");
        let permit = session
            .try_acquire(now)
            .expect("admission state should be available")
            .expect("probe should acquire its permit");
        let runner = FfprobeRunner::new(binary, Duration::from_secs(60));
        let caller = tokio::spawn(async move {
            runner
                .probe_path_with_permit(Path::new("/tmp/fixture.mkv"), permit)
                .await
        });

        timeout(Duration::from_secs(5), async {
            while !marker.exists() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake ffprobe should start");
        assert_eq!(runtime.active_count().expect("active count"), 1);

        caller.abort();
        assert!(
            caller
                .await
                .expect_err("caller task should be cancelled")
                .is_cancelled()
        );
        timeout(Duration::from_secs(5), async {
            while runtime.active_count().expect("active count") != 0 {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("child cleanup should release the permit after reaping");
    }

    fn shell_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
