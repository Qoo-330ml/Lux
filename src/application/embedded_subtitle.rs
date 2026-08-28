use std::{
    fmt,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    sync::Semaphore,
    time::timeout,
};

use crate::storage::StoredSubtitleStream;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_OUTPUT_BYTES: usize = 1_048_576;
const DEFAULT_CONCURRENCY: usize = 4;

#[derive(Debug)]
pub(crate) enum EmbeddedSubtitleError {
    InvalidSource,
    UnsupportedFormat,
    Missing,
    Forbidden,
    Limit,
    Timeout,
    Spawn,
    Io,
    ProcessFailed,
}

impl fmt::Display for EmbeddedSubtitleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidSource => "本地内嵌字幕不可用",
            Self::UnsupportedFormat => "字幕格式不受支持",
            Self::Missing => "字幕媒体不存在",
            Self::Forbidden => "字幕媒体路径不可用",
            Self::Limit => "字幕内容超过大小限制",
            Self::Timeout => "字幕提取超时",
            Self::Spawn | Self::Io | Self::ProcessFailed => "字幕提取失败",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for EmbeddedSubtitleError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EmbeddedSubtitleResult {
    pub bytes: Vec<u8>,
    pub format: &'static str,
    pub content_type: &'static str,
}

#[derive(Clone)]
pub(crate) struct EmbeddedSubtitleService {
    ffmpeg_executable: PathBuf,
    timeout: Duration,
    max_output_bytes: usize,
    permits: Arc<Semaphore>,
}

impl EmbeddedSubtitleService {
    pub(crate) fn new() -> Self {
        Self::with_limits(
            std::env::var_os("LUX_FFMPEG_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("ffmpeg")),
            DEFAULT_TIMEOUT,
            DEFAULT_MAX_OUTPUT_BYTES,
            DEFAULT_CONCURRENCY,
        )
    }

    pub(crate) fn with_executable(ffmpeg_executable: PathBuf) -> Self {
        Self::with_limits(
            ffmpeg_executable,
            DEFAULT_TIMEOUT,
            DEFAULT_MAX_OUTPUT_BYTES,
            DEFAULT_CONCURRENCY,
        )
    }

    fn with_limits(
        ffmpeg_executable: PathBuf,
        timeout: Duration,
        max_output_bytes: usize,
        concurrency: usize,
    ) -> Self {
        Self {
            ffmpeg_executable,
            timeout,
            max_output_bytes,
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
        }
    }

    pub(crate) async fn extract(
        &self,
        stream: &StoredSubtitleStream,
    ) -> Result<EmbeddedSubtitleResult, EmbeddedSubtitleError> {
        let (format, muxer, content_type) = subtitle_format(stream)?;
        if stream.source_kind != "LOCAL_FILE"
            || stream.stream_type != "SUBTITLE"
            || stream.is_external
            || stream.external_path.is_some()
            || stream.stream_index < 0
            || stream.probe_status != "READY"
        {
            return Err(EmbeddedSubtitleError::InvalidSource);
        }
        let path = canonical_media_path(&stream.root_path, &stream.relative_path).await?;
        let _permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| EmbeddedSubtitleError::Limit)?;
        let mut command = Command::new(&self.ffmpeg_executable);
        let map = format!("0:{}", stream.stream_index);
        command
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
            .arg(path)
            .args(["-map", map.as_str(), "-c:s", "copy", "-f", muxer, "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| EmbeddedSubtitleError::Spawn)?;
        let mut stdout = child.stdout.take().ok_or(EmbeddedSubtitleError::Spawn)?;
        let Some(mut stderr) = child.stderr.take() else {
            terminate_child(&mut child).await;
            return Err(EmbeddedSubtitleError::Spawn);
        };
        let stderr_task = tokio::spawn(async move {
            let mut buffer = [0_u8; 4096];
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });
        let result = timeout(self.timeout, async {
            let bytes = read_bounded(&mut stdout, self.max_output_bytes).await?;
            let status = child.wait().await.map_err(|_| EmbeddedSubtitleError::Io)?;
            Ok::<_, EmbeddedSubtitleError>((bytes, status.success()))
        })
        .await;
        let result = match result {
            Ok(result) => result,
            Err(_) => {
                terminate_child(&mut child).await;
                stderr_task.abort();
                let _ = stderr_task.await;
                return Err(EmbeddedSubtitleError::Timeout);
            }
        };
        stderr_task.abort();
        let _ = stderr_task.await;
        match result {
            Ok((bytes, true)) => Ok(EmbeddedSubtitleResult {
                bytes,
                format,
                content_type,
            }),
            Ok((_, false)) => Err(EmbeddedSubtitleError::ProcessFailed),
            Err(error) => {
                terminate_child(&mut child).await;
                Err(error)
            }
        }
    }
}

impl Default for EmbeddedSubtitleService {
    fn default() -> Self {
        Self::new()
    }
}

fn subtitle_format(
    stream: &StoredSubtitleStream,
) -> Result<(&'static str, &'static str, &'static str), EmbeddedSubtitleError> {
    match stream
        .codec
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("srt" | "subrip") => Ok(("srt", "srt", "text/plain; charset=utf-8")),
        Some("ass" | "ssa") => Ok(("ass", "ass", "text/x-ass; charset=utf-8")),
        _ => Err(EmbeddedSubtitleError::UnsupportedFormat),
    }
}

async fn canonical_media_path(
    root_path: &str,
    relative_path: &str,
) -> Result<PathBuf, EmbeddedSubtitleError> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(EmbeddedSubtitleError::Forbidden);
    }
    let root = fs::canonicalize(root_path)
        .await
        .map_err(|_| EmbeddedSubtitleError::Missing)?;
    let path = fs::canonicalize(root.join(relative))
        .await
        .map_err(|_| EmbeddedSubtitleError::Missing)?;
    if !path.starts_with(&root) || path == root {
        return Err(EmbeddedSubtitleError::Forbidden);
    }
    let metadata = fs::metadata(&path)
        .await
        .map_err(|_| EmbeddedSubtitleError::Missing)?;
    if !metadata.is_file() {
        return Err(EmbeddedSubtitleError::Forbidden);
    }
    Ok(path)
}

async fn read_bounded<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> Result<Vec<u8>, EmbeddedSubtitleError> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|_| EmbeddedSubtitleError::Io)?;
        if read == 0 {
            return Ok(output);
        }
        if output.len().saturating_add(read) > max_bytes {
            return Err(EmbeddedSubtitleError::Limit);
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

async fn terminate_child(child: &mut Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

    use crate::storage::StoredSubtitleStream;

    use super::EmbeddedSubtitleService;

    fn stream(source_kind: &str, codec: &str) -> StoredSubtitleStream {
        StoredSubtitleStream {
            media_source_id: "source-1".to_owned(),
            item_id: "item-1".to_owned(),
            source_kind: source_kind.to_owned(),
            probe_status: "READY".to_owned(),
            root_path: String::new(),
            relative_path: String::new(),
            stream_index: 2,
            stream_type: "SUBTITLE".to_owned(),
            codec: Some(codec.to_owned()),
            language: Some("eng".to_owned()),
            title: Some("English".to_owned()),
            details_json: None,
            external_path: None,
            is_external: false,
            is_default: false,
            is_forced: false,
        }
    }

    fn executable_script(directory: &std::path::Path, body: &str) -> PathBuf {
        let path = directory.join("ffmpeg");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("fake ffmpeg");
        let mut permissions = fs::metadata(&path).expect("fake metadata").permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&path, permissions).expect("fake permissions");
        path
    }

    fn local_stream(root: &std::path::Path, codec: &str) -> StoredSubtitleStream {
        let mut stream = stream("LOCAL_FILE", codec);
        stream.root_path = root.to_string_lossy().into_owned();
        stream.relative_path = "movie.mkv".to_owned();
        stream
    }

    #[tokio::test]
    async fn extracts_a_bounded_local_text_subtitle() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(directory.path().join("movie.mkv"), b"fixture").expect("movie");
        let ffmpeg = executable_script(
            directory.path(),
            "printf '1\\n00:00:00,000 --> 00:00:01,000\\nHello\\n'",
        );
        let service = EmbeddedSubtitleService::with_limits(ffmpeg, Duration::from_secs(2), 1024, 1);

        let result = service
            .extract(&local_stream(directory.path(), "subrip"))
            .await
            .expect("subtitle");

        assert_eq!(result.format, "srt");
        assert_eq!(result.content_type, "text/plain; charset=utf-8");
        assert_eq!(result.bytes, b"1\n00:00:00,000 --> 00:00:01,000\nHello\n");
    }

    #[tokio::test]
    async fn rejects_remote_and_graphic_subtitles_before_starting_ffmpeg() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let marker = directory.path().join("started");
        let ffmpeg = executable_script(directory.path(), &format!("touch '{}'", marker.display()));
        let service = EmbeddedSubtitleService::with_limits(ffmpeg, Duration::from_secs(2), 1024, 1);

        let remote = service
            .extract(&stream("STRM_URL", "subrip"))
            .await
            .expect_err("remote subtitle must be rejected");
        assert_eq!(remote.to_string(), "本地内嵌字幕不可用");
        let graphic = service
            .extract(&local_stream(directory.path(), "hdmv_pgs_subtitle"))
            .await
            .expect_err("graphic subtitle must be rejected");
        assert_eq!(graphic.to_string(), "字幕格式不受支持");
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn rejects_excessive_extracted_output() {
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(directory.path().join("movie.mkv"), b"fixture").expect("movie");
        let ffmpeg = executable_script(
            directory.path(),
            "awk 'BEGIN { for (i = 0; i < 2048; i++) printf \"x\" }'",
        );
        let service = EmbeddedSubtitleService::with_limits(ffmpeg, Duration::from_secs(2), 1024, 1);

        let error = service
            .extract(&local_stream(directory.path(), "ass"))
            .await
            .expect_err("output must be bounded");

        assert_eq!(error.to_string(), "字幕内容超过大小限制");
    }
}
