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
        let runner = self.clone();
        let path = path.to_owned();
        let (sender, receiver) = oneshot::channel();
        let _ = tokio::spawn(async move {
            let mut sender = sender;
            let result = runner.run_path(&path, &mut sender).await;
            let _ = sender.send(result);
        });
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
    ) -> Result<MediaProbeResult, ProbeError> {
        let mut child = Command::new(&self.binary)
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
            .spawn()
            .map_err(ProbeError::Io)?;
        let output = tokio::select! {
            _ = response.closed() => None,
            output = timeout(self.timeout, collect_process_output(&mut child)) => Some(output),
        };
        let output = match output {
            None => {
                terminate_child(child).await;
                return Err(ProbeError::Io(std::io::Error::other(
                    "ffprobe process canceled before returning a result",
                )));
            }
            Some(Ok(Ok(output))) => output,
            Some(Ok(Err(error))) => {
                terminate_child(child).await;
                return Err(error);
            }
            Some(Err(_)) => {
                terminate_child(child).await;
                return Err(ProbeError::Timeout);
            }
        };
        if !output.status.success() {
            return Err(ProbeError::Exit {
                code: output.status.code(),
                stderr: truncate(&output.stderr),
            });
        }
        parse_probe_json(&output.stdout)
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

async fn terminate_child(child: Child) {
    terminate_child_with_wait(child, |child| async move {
        let mut child = child;
        let result = child.wait().await;
        (child, result)
    })
    .await;
}

async fn terminate_child_with_wait<F, Fut>(mut child: Child, mut wait: F)
where
    F: FnMut(Child) -> Fut,
    Fut: Future<Output = (Child, std::io::Result<std::process::ExitStatus>)>,
{
    let _ = child.start_kill();
    let mut next_warning = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let (next_child, result) = wait(child).await;
        child = next_child;
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
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use tokio::{
        process::Command,
        sync::oneshot,
        time::{Duration, sleep},
    };

    use super::terminate_child_with_wait;

    struct TestPermit(Arc<AtomicBool>);

    impl Drop for TestPermit {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn process_permit_stays_held_until_wait_confirms_child_exit() {
        let child = Command::new("sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .expect("sleep process should start");
        let permit_held = Arc::new(AtomicBool::new(true));
        let owner_permit = Arc::clone(&permit_held);
        let (wait_started_tx, wait_started_rx) = oneshot::channel();
        let (release_wait_tx, release_wait_rx) = oneshot::channel();
        let owner = tokio::spawn(async move {
            let _permit = TestPermit(owner_permit);
            let mut wait_started_tx = Some(wait_started_tx);
            let mut release_wait_rx = Some(release_wait_rx);
            terminate_child_with_wait(child, move |child| {
                let wait_started_tx = wait_started_tx.take();
                let release_wait_rx = release_wait_rx.take();
                async move {
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
                    let mut child = child;
                    let result = match gate_result {
                        Ok(()) => child.wait().await,
                        Err(error) => Err(error),
                    };
                    (child, result)
                }
            })
            .await;
        });

        wait_started_rx
            .await
            .expect("cleanup should reach child wait");
        sleep(Duration::from_millis(1_100)).await;
        assert!(
            !owner.is_finished(),
            "owner must not finish while child reaping is unconfirmed"
        );
        assert!(permit_held.load(Ordering::SeqCst));

        let _ = release_wait_tx.send(());
        owner.await.expect("child owner should finish after wait");
        assert!(!permit_held.load(Ordering::SeqCst));
    }
}
