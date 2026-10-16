#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use luxd::{
    application::{
        libraries::LibraryService,
        metadata::MetadataEnricher,
        probe::{FfprobeRunner, MediaProbeService},
        scanner::LibraryScanner,
        thumbnails::ThumbnailService,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};

fn config(root: &Path) -> Config {
    Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address is valid"),
        config_dir: root.join("config"),
    }
}

fn executable_script(path: &Path, body: &str) -> Result<(), Box<dyn std::error::Error>> {
    fs::write(path, body)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

async fn wait_for_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !tokio::fs::try_exists(path).await.unwrap_or(false) {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn probe_and_thumbnail_services_share_budget_across_libraries()
-> Result<(), Box<dyn std::error::Error>> {
    // SAFETY: this is the only test in its integration-test process, it runs on
    // a current-thread runtime, and no budgeted service is created before this.
    unsafe { std::env::set_var("LUX_BACKGROUND_CPU_BUDGET", "1") };

    let temp_dir = tempfile::tempdir()?;
    let probe_root = temp_dir.path().join("Probe Movies");
    let probe_movie = probe_root.join("Probe Fixture (2024)");
    fs::create_dir_all(&probe_movie)?;
    fs::write(probe_movie.join("Probe.Fixture.2024.mkv"), b"probe fixture")?;
    fs::write(
        probe_movie.join("movie.nfo"),
        "<movie><title>Probe Fixture</title><year>2024</year></movie>",
    )?;

    let thumbnail_root = temp_dir.path().join("Thumbnail Movies");
    let thumbnail_movie = thumbnail_root.join("Thumbnail Fixture (2024)");
    fs::create_dir_all(&thumbnail_movie)?;
    fs::write(
        thumbnail_movie.join("Thumbnail.Fixture.2024.mkv"),
        b"thumbnail fixture",
    )?;

    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let probe_library = libraries
        .create_library("Probe Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            probe_library.id,
            probe_root.to_str().ok_or("non-UTF8 probe root")?,
        )
        .await?;
    let thumbnail_library = libraries
        .create_library("Thumbnail Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            thumbnail_library.id,
            thumbnail_root.to_str().ok_or("non-UTF8 thumbnail root")?,
        )
        .await?;

    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(probe_library.id).await?;
    scanner.scan_movie_library(thumbnail_library.id).await?;

    let state_dir = temp_dir.path().join("shared-state");
    fs::create_dir_all(&state_dir)?;
    let probe_binary = temp_dir.path().join("fake-ffprobe");
    executable_script(
        &probe_binary,
        "#!/bin/sh\nset -eu\nstate=\"$(dirname \"$0\")/shared-state\"\ntouch \"$state/probe.active\" \"$state/probe.started\"\nwhile [ ! -f \"$state/probe.release\" ]; do sleep 0.01; done\nprintf '%s' '{\"format\":{\"format_name\":\"matroska\"},\"streams\":[]}'\nsleep 0.2\nrm -f \"$state/probe.active\"\n",
    )?;
    let ffmpeg_binary = temp_dir.path().join("fake-ffmpeg");
    executable_script(
        &ffmpeg_binary,
        "#!/bin/sh\nset -eu\nstate=\"$(dirname \"$0\")/shared-state\"\nif [ -f \"$state/probe.active\" ]; then touch \"$state/overlap\"; fi\ntouch \"$state/thumbnail.started\"\nwhile [ ! -f \"$state/thumbnail.release\" ]; do sleep 0.01; done\nfor argument in \"$@\"; do case \"$argument\" in *.jpg) printf '\\377\\330lux-thumb\\377\\331' > \"$argument\";; esac; done\n",
    )?;

    let probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(probe_binary, Duration::from_secs(10)),
    );
    let probe_library_id = probe_library.id;
    let probe_task = tokio::spawn(async move { probe.probe_movie_library(probe_library_id).await });
    let probe_started = state_dir.join("probe.started");
    wait_for_file(&probe_started).await?;

    let thumbnails =
        ThumbnailService::with_runner(database.clone(), ffmpeg_binary, Duration::from_secs(10));
    let thumbnail_library_id = thumbnail_library.id;
    let thumbnail_task =
        tokio::spawn(async move { thumbnails.generate_library(thumbnail_library_id).await });

    let metadata = MetadataEnricher::new(database);
    let metadata_library_id = probe_library.id;
    let mut metadata_task =
        tokio::spawn(async move { metadata.enrich_movie_library(metadata_library_id).await });
    let metadata_result = tokio::time::timeout(Duration::from_secs(1), &mut metadata_task).await;
    let metadata_report = match metadata_result {
        Ok(Ok(Ok(report))) => report,
        _ => {
            fs::write(state_dir.join("probe.release"), b"")?;
            fs::write(state_dir.join("thumbnail.release"), b"")?;
            let _ = metadata_task.await;
            let _ = probe_task.await;
            let _ = thumbnail_task.await;
            return Err("foreground metadata waited on the background CPU budget".into());
        }
    };
    assert_eq!(metadata_report.items_processed, 1);

    let thumbnail_started = state_dir.join("thumbnail.started");
    let thumbnail_started_while_probe_held = tokio::time::timeout(
        Duration::from_millis(250),
        wait_for_file(&thumbnail_started),
    )
    .await
    .is_ok();

    if thumbnail_started_while_probe_held {
        fs::write(state_dir.join("probe.release"), b"")?;
        fs::write(state_dir.join("thumbnail.release"), b"")?;
        let _ = probe_task.await;
        let _ = thumbnail_task.await;
        return Err("thumbnail process overlapped a probe holding the G=1 budget".into());
    }

    fs::write(state_dir.join("probe.release"), b"")?;
    let probe_report = tokio::time::timeout(Duration::from_secs(5), probe_task).await???;
    assert_eq!(probe_report.ready, 1);
    assert_eq!(probe_report.failed, 0);

    wait_for_file(&thumbnail_started).await?;
    assert!(!state_dir.join("overlap").exists());
    fs::write(state_dir.join("thumbnail.release"), b"")?;
    let thumbnail_report = tokio::time::timeout(Duration::from_secs(5), thumbnail_task).await???;
    assert_eq!(thumbnail_report.generated, 1);
    assert!(!state_dir.join("overlap").exists());
    Ok(())
}
