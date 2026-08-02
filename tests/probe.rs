use std::{path::Path, time::Duration};

use luxd::{
    application::{
        libraries::LibraryService,
        probe::{
            FfprobeRunner, MediaProbeResult, MediaProbeService, MediaStreamResult, ProbeError,
            StreamType, parse_probe_json,
        },
        scanner::LibraryScanner,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[test]
fn probe_json_keeps_container_duration_bitrate_and_media_streams() {
    let result = parse_probe_json(
        br#"{
            "format": {"format_name": "matroska,webm", "duration": "120.5", "bit_rate": "800000"},
            "streams": [
                {"index": 0, "codec_type": "video", "codec_name": "h264"},
                {"index": 1, "codec_type": "audio", "codec_name": "aac", "tags": {"language": "eng", "title": "English"}},
                {"index": 2, "codec_type": "subtitle", "codec_name": "subrip", "tags": {"language": "chi"}},
                {"index": 3, "codec_type": "data", "codec_name": "bin_data"}
            ]
        }"#,
    )
    .expect("valid ffprobe json");

    assert_eq!(
        result,
        MediaProbeResult {
            container: Some("matroska,webm".to_owned()),
            duration_ticks: Some(1_205_000_000),
            bitrate: Some(800_000),
            streams: vec![
                MediaStreamResult {
                    stream_index: 0,
                    stream_type: StreamType::Video,
                    codec: Some("h264".to_owned()),
                    language: None,
                    title: None,
                },
                MediaStreamResult {
                    stream_index: 1,
                    stream_type: StreamType::Audio,
                    codec: Some("aac".to_owned()),
                    language: Some("eng".to_owned()),
                    title: Some("English".to_owned()),
                },
                MediaStreamResult {
                    stream_index: 2,
                    stream_type: StreamType::Subtitle,
                    codec: Some("subrip".to_owned()),
                    language: Some("chi".to_owned()),
                    title: None,
                },
            ],
        }
    );
}

#[test]
fn malformed_probe_json_is_rejected() {
    assert!(matches!(
        parse_probe_json(br#"{"format":{"duration":"not-a-number"}}"#),
        Err(ProbeError::InvalidOutput(_))
    ));
}

#[test]
fn duplicate_probe_stream_indexes_are_rejected() {
    assert!(matches!(
        parse_probe_json(
            br#"{"streams":[{"index":0,"codec_type":"video"},{"index":0,"codec_type":"audio"}]}"#,
        ),
        Err(ProbeError::InvalidOutput(_))
    ));
}

#[test]
fn unavailable_optional_probe_values_do_not_discard_streams() {
    let result = parse_probe_json(
        br#"{"format":{"format_name":"mpeg4","duration":"N/A","bit_rate":"N/A"},"streams":[]}"#,
    )
    .expect("valid probe with unavailable optional values");
    assert_eq!(result.duration_ticks, None);
    assert_eq!(result.bitrate, None);
}

#[tokio::test]
async fn probe_runner_classifies_timeout() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let script = executable_script(temp_dir.path(), "#!/bin/sh\nsleep 1\n")?;
    let runner = FfprobeRunner::new(script, Duration::from_millis(20));

    let error = runner
        .probe_path(Path::new("/tmp/fixture.mkv"))
        .await
        .expect_err("probe should time out");
    assert!(matches!(error, ProbeError::Timeout));
    Ok(())
}

#[tokio::test]
async fn probe_service_persists_success_skips_ready_and_reprobes_changed_file()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config_for(&temp_dir);
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Probe Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    let movie_path = movie_dir.join("Probe.Movie.2024.mkv");
    tokio::fs::write(&movie_path, b"fixture").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(library.id).await?;

    let script = executable_script(
        temp_dir.path(),
        r#"#!/bin/sh
printf '%s' '{"format":{"format_name":"matroska","duration":"12.5","bit_rate":"500000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264"},{"index":1,"codec_type":"audio","codec_name":"aac","tags":{"language":"eng"}}]}'
"#,
    )?;
    let service = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(script, Duration::from_secs(1)),
    );

    let first = service.probe_movie_library(library.id).await?;
    assert_eq!(first.attempted, 1);
    assert_eq!(first.ready, 1);
    assert_eq!(first.failed, 0);
    assert_eq!(first.timed_out, 0);

    let source: (String, i64, i64, i64, Option<String>) = sqlx::query_as(
        "SELECT container, duration_ticks, bitrate, 
                (SELECT COUNT(*) FROM media_streams WHERE media_source_id = media_sources.id),
                probe_error
         FROM media_sources",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        source,
        ("matroska".to_owned(), 125_000_000, 500_000, 2, None)
    );

    let second = service.probe_movie_library(library.id).await?;
    assert_eq!(second.attempted, 0);
    assert_eq!(second.skipped, 1);

    tokio::fs::write(&movie_path, b"fixture changed").await?;
    let changed = scanner.scan_movie_library(library.id).await?;
    assert_eq!(changed.changed_files, 1);
    assert_eq!(changed.created_sources, 0);

    let third = service.probe_movie_library(library.id).await?;
    assert_eq!(third.attempted, 1);
    assert_eq!(third.ready, 1);
    Ok(())
}

#[tokio::test]
async fn probe_service_persists_exit_failure_without_retrying_automatically()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config_for(&temp_dir);
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Broken Probe (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Broken.Probe.2024.mkv"), b"fixture").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let script = executable_script(temp_dir.path(), "#!/bin/sh\necho broken >&2\nexit 7\n")?;
    let service = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(script, Duration::from_secs(1)),
    );
    let first = service.probe_movie_library(library.id).await?;
    assert_eq!(first.failed, 1);
    let status: (String, String) =
        sqlx::query_as("SELECT probe_status, probe_error FROM media_sources")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(status.0, "FAILED");
    assert!(status.1.contains("exited"));

    let second = service.probe_movie_library(library.id).await?;
    assert_eq!(second.attempted, 0);
    assert_eq!(second.skipped, 1);
    Ok(())
}

#[tokio::test]
async fn probe_service_persists_timeout_status() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config_for(&temp_dir);
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Timeout Probe (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Timeout.Probe.2024.mkv"), b"fixture").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let script = executable_script(temp_dir.path(), "#!/bin/sh\nsleep 1\n")?;
    let service = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(script, Duration::from_millis(20)),
    );
    let report = service.probe_movie_library(library.id).await?;
    assert_eq!(report.timed_out, 1);
    let status: (String, String) =
        sqlx::query_as("SELECT probe_status, probe_error FROM media_sources")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(status.0, "TIMEOUT");
    assert!(status.1.contains("timed out"));
    Ok(())
}

fn config_for(temp_dir: &tempfile::TempDir) -> Config {
    Config {
        http_addr: "127.0.0.1:8097".parse().expect("address"),
        config_dir: temp_dir.path().join("config"),
    }
}

fn executable_script(
    directory: &Path,
    contents: &str,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    use std::{fs, os::unix::fs::PermissionsExt};

    let path = directory.join(format!("fake-ffprobe-{}", uuid::Uuid::now_v7()));
    fs::write(&path, contents)?;
    let mut permissions = fs::metadata(&path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions)?;
    Ok(path)
}
