#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use luxd::{
    application::{
        libraries::LibraryService, scanner::ScanJobService, thumbnails::ThumbnailService,
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

fn fake_ffmpeg(
    path: &Path,
    log_path: &Path,
    exit_code: i32,
) -> Result<(), Box<dyn std::error::Error>> {
    let command = if exit_code == 0 {
        "printf '\\377\\330lux-thumb\\377\\331' > \"$output\"".to_owned()
    } else {
        format!("exit {exit_code}")
    };
    let script = format!(
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" >> '{}'\noutput=''\nfor argument in \"$@\"; do output=\"$argument\"; done\n{}\n",
        log_path.display(),
        command
    );
    fs::write(path, script)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[tokio::test]
async fn scan_generates_local_thumbnail_but_never_strm_thumbnail()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Local Movie (2024)");
    fs::create_dir_all(&movie_dir)?;
    fs::write(movie_dir.join("Local.Movie.2024.mkv"), b"video")?;
    fs::write(
        movie_dir.join("Remote.strm"),
        "https://example.invalid/video",
    )?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;

    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    fake_ffmpeg(&fake, &log, 0)?;
    let thumbnails = ThumbnailService::with_runner(database.clone(), fake, Duration::from_secs(5));
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion_with_metadata_and_thumbnails(&job.id, 100, None, None, Some(thumbnails))
        .await?;

    assert!(movie_dir.join("Local.Movie.2024-thumb.jpg").is_file());
    assert!(!movie_dir.join("Remote-thumb.jpg").exists());
    let image_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM item_images WHERE image_type = 'THUMB'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(image_count, 1);
    let ffmpeg_arguments = fs::read_to_string(log)?;
    assert!(!ffmpeg_arguments.contains("Remote.strm"));
    Ok(())
}

#[tokio::test]
async fn existing_thumbnail_is_not_overwritten() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Local Movie (2024)");
    fs::create_dir_all(&movie_dir)?;
    fs::write(movie_dir.join("Local.Movie.2024.mkv"), b"video")?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;

    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    fake_ffmpeg(&fake, &log, 0)?;
    let thumbnails = ThumbnailService::with_runner(database.clone(), fake, Duration::from_secs(5));
    let jobs = ScanJobService::new(database.clone());
    let first_job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion_with_metadata_and_thumbnails(
        &first_job.id,
        100,
        None,
        None,
        Some(thumbnails.clone()),
    )
    .await?;
    let target = movie_dir.join("Local.Movie.2024-thumb.jpg");
    let original = fs::read(&target)?;
    let log_after_first_run = fs::read_to_string(&log)?;

    let second_job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion_with_metadata_and_thumbnails(
        &second_job.id,
        100,
        None,
        None,
        Some(thumbnails),
    )
    .await?;
    assert_eq!(fs::read(target)?, original);
    assert_eq!(fs::read_to_string(log)?, log_after_first_run);
    Ok(())
}

#[tokio::test]
async fn existing_series_episode_thumbnail_is_indexed_before_thumbnail_generation()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root = temp_dir.path().join("Shows");
    let season_dir = root.join("Example Show (2024)").join("Season 01");
    fs::create_dir_all(&season_dir)?;
    fs::write(season_dir.join("Example.Show.S01E01.mkv"), b"video")?;
    fs::write(
        season_dir.join("Example.Show.S01E01-thumb.jpg"),
        b"existing-thumbnail",
    )?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;

    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    fs::write(&log, "")?;
    fake_ffmpeg(&fake, &log, 0)?;
    let thumbnails = ThumbnailService::with_runner(database.clone(), fake, Duration::from_secs(5));
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion_with_metadata_and_thumbnails(&job.id, 100, None, None, Some(thumbnails))
        .await?;

    assert_eq!(fs::read_to_string(log)?, "");
    let image_row: (String, String) = sqlx::query_as(
        "SELECT ii.image_type, ii.source
         FROM item_images ii
         JOIN media_items mi ON mi.id = ii.item_id
         WHERE mi.item_type = 'EPISODE'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(image_row, ("THUMB".to_owned(), "LOCAL".to_owned()));
    Ok(())
}

#[tokio::test]
async fn ffmpeg_failure_makes_scan_retryable() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Broken Movie (2024)");
    fs::create_dir_all(&movie_dir)?;
    fs::write(movie_dir.join("Broken.Movie.2024.mkv"), b"video")?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;

    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    fake_ffmpeg(&fake, &log, 7)?;
    let thumbnails = ThumbnailService::with_runner(database.clone(), fake, Duration::from_secs(5));
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion_with_metadata_and_thumbnails(&job.id, 100, None, None, Some(thumbnails))
        .await?;

    let status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(status, "FAILED");
    let event: (String, String) = sqlx::query_as(
        "SELECT level, event_code FROM scan_job_events
         WHERE job_id = ? AND event_code = 'THUMBNAIL_FAILED'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(event, ("WARN".to_owned(), "THUMBNAIL_FAILED".to_owned()));
    Ok(())
}
