#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use luxd::{
    application::{
        libraries::LibraryService,
        scanner::{LibraryScanner, ScanJobService},
        thumbnails::ThumbnailService,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use uuid::Uuid;

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
        "for argument in \"$@\"; do case \"$argument\" in *.jpg) printf '\\377\\330lux-thumb\\377\\331' > \"$argument\";; esac; done".to_owned()
    } else {
        format!("exit {exit_code}")
    };
    let script = format!(
        "#!/bin/sh\nset -eu\nprintf '%s\\n' -- >> '{}'\nprintf '%s\\n' \"$@\" >> '{}'\n{}\n",
        log_path.display(),
        log_path.display(),
        command
    );
    fs::write(path, script)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn fake_ffmpeg_rejecting_multi_output(
    path: &Path,
    log_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let script = format!(
        "#!/bin/sh\nset -eu\nprintf '%s\\n' -- >> '{}'\nprintf '%s\\n' \"$@\" >> '{}'\nfor argument in \"$@\"; do\n  if [ \"$argument\" = '-filter_complex' ]; then exit 42; fi\ndone\nfor argument in \"$@\"; do case \"$argument\" in *.jpg) printf '\\377\\330lux-thumb\\377\\331' > \"$argument\";; esac; done\n",
        log_path.display(),
        log_path.display(),
    );
    fs::write(path, script)?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn ffmpeg_invocation_count(log: &str) -> usize {
    log.lines().filter(|line| *line == "--").count()
}

#[tokio::test]
async fn missing_poster_and_thumbnail_use_one_ffmpeg_invocation()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Dual Output (2024)");
    fs::create_dir_all(&movie_dir)?;
    fs::write(movie_dir.join("Dual.Output.2024.mkv"), b"video")?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let scan = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&scan.id, 100, None).await?;

    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    fake_ffmpeg(&fake, &log, 0)?;
    let thumbnails = ThumbnailService::with_runner(database.clone(), fake, Duration::from_secs(5));
    let report = thumbnails.generate_library(library.id).await?;

    assert_eq!(report.generated, 1);
    assert!(movie_dir.join("Dual.Output.2024-poster.jpg").is_file());
    assert!(movie_dir.join("Dual.Output.2024-thumbnail.jpg").is_file());
    let image_rows: Vec<(String, String)> =
        sqlx::query_as("SELECT image_type, source FROM item_images ORDER BY image_type")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(
        image_rows,
        vec![
            ("POSTER".to_owned(), "FFMPEG".to_owned()),
            ("THUMB".to_owned(), "FFMPEG".to_owned()),
        ]
    );
    let log = fs::read_to_string(log)?;
    assert_eq!(ffmpeg_invocation_count(&log), 1, "ffmpeg log:\n{log}");
    assert!(log.contains("-filter_complex"), "ffmpeg log:\n{log}");
    assert!(log.contains("scale=600:900"), "ffmpeg log:\n{log}");
    assert!(log.contains("scale=1280:720"), "ffmpeg log:\n{log}");
    Ok(())
}

#[tokio::test]
async fn unsupported_dual_output_falls_back_to_two_single_outputs()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Fallback (2024)");
    fs::create_dir_all(&movie_dir)?;
    fs::write(movie_dir.join("Fallback.2024.mkv"), b"video")?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let scan = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&scan.id, 100, None).await?;

    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    fake_ffmpeg_rejecting_multi_output(&fake, &log)?;
    let thumbnails = ThumbnailService::with_runner(database, fake, Duration::from_secs(5));
    let report = thumbnails.generate_library(library.id).await?;

    assert_eq!(report.generated, 1);
    assert!(movie_dir.join("Fallback.2024-poster.jpg").is_file());
    assert!(movie_dir.join("Fallback.2024-thumbnail.jpg").is_file());
    let log = fs::read_to_string(log)?;
    assert_eq!(ffmpeg_invocation_count(&log), 3, "ffmpeg log:\n{log}");
    assert!(log.contains("-filter_complex"), "ffmpeg log:\n{log}");
    Ok(())
}

#[tokio::test]
async fn failed_dual_output_cleans_temporary_and_final_files()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Broken Dual (2024)");
    fs::create_dir_all(&movie_dir)?;
    fs::write(movie_dir.join("Broken.Dual.2024.mkv"), b"video")?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let scan = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&scan.id, 100, None).await?;

    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    let script = format!(
        "#!/bin/sh\nset -eu\nprintf '%s\\n' -- >> '{}'\nprintf '%s\\n' \"$@\" >> '{}'\nfor argument in \"$@\"; do case \"$argument\" in *-POSTER.tmp.jpg) printf '\\377\\330lux-thumb\\377\\331' > \"$argument\";; esac; done\nexit 0\n",
        log_path_placeholder(),
        log_path_placeholder(),
    );
    let script = script.replace(log_path_placeholder(), &log.to_string_lossy());
    fs::write(&fake, script)?;
    let mut permissions = fs::metadata(&fake)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake, permissions)?;
    let thumbnails = ThumbnailService::with_runner(database.clone(), fake, Duration::from_secs(5));
    let report = thumbnails.generate_library(library.id).await?;

    assert_eq!(report.generated, 0);
    assert_eq!(report.failed, 1);
    assert!(!movie_dir.join("Broken.Dual.2024-poster.jpg").exists());
    assert!(!movie_dir.join("Broken.Dual.2024-thumbnail.jpg").exists());
    let temporary_count = fs::read_dir(&movie_dir)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(".lux-"))
        .count();
    assert_eq!(temporary_count, 0);
    let image_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_images")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(image_count, 0);
    Ok(())
}

fn log_path_placeholder() -> &'static str {
    "__LUX_THUMBNAIL_LOG_PATH__"
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

    assert!(movie_dir.join("Local.Movie.2024-poster.jpg").is_file());
    assert!(movie_dir.join("Local.Movie.2024-thumbnail.jpg").is_file());
    assert!(!movie_dir.join("Remote-thumb.jpg").exists());
    let canonical_movie_dir = fs::canonicalize(&movie_dir)?;
    let image_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT image_type, local_path
         FROM item_images
         ORDER BY image_type",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        image_rows,
        vec![
            (
                "POSTER".to_owned(),
                canonical_movie_dir
                    .join("Local.Movie.2024-poster.jpg")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "THUMB".to_owned(),
                canonical_movie_dir
                    .join("Local.Movie.2024-thumbnail.jpg")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ]
    );
    let ffmpeg_arguments = fs::read_to_string(log)?;
    assert_eq!(ffmpeg_invocation_count(&ffmpeg_arguments), 1);
    assert!(ffmpeg_arguments.contains("scale=600:900"));
    assert!(ffmpeg_arguments.contains("scale=1280:720"));
    assert!(!ffmpeg_arguments.contains("Remote.strm"));
    Ok(())
}

#[tokio::test]
async fn full_scan_generates_missing_thumbnails_for_existing_media_after_mode_change()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Existing Movie (2024)");
    fs::create_dir_all(&movie_dir)?;
    fs::write(movie_dir.join("Existing.Movie.2024.mkv"), b"video")?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial_scan = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial_scan.id, 100, None).await?;

    sqlx::query("UPDATE libraries SET media_strategy_json = ? WHERE id = ?")
        .bind(r#"{"images":{"thumbnailScrapingMode":"SCREENSHOT_FIRST"}}"#)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    fake_ffmpeg(&fake, &log, 0)?;
    let thumbnails = ThumbnailService::with_runner(database.clone(), fake, Duration::from_secs(5));
    let second_scan = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion_with_metadata_and_thumbnails(
        &second_scan.id,
        100,
        None,
        None,
        Some(thumbnails),
    )
    .await?;

    assert!(movie_dir.join("Existing.Movie.2024-poster.jpg").is_file());
    assert!(
        movie_dir
            .join("Existing.Movie.2024-thumbnail.jpg")
            .is_file()
    );
    let image_sources: Vec<(String, String)> =
        sqlx::query_as("SELECT image_type, source FROM item_images ORDER BY image_type")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(
        image_sources,
        vec![
            ("POSTER".to_owned(), "FFMPEG".to_owned()),
            ("THUMB".to_owned(), "FFMPEG".to_owned()),
        ]
    );
    assert_eq!(ffmpeg_invocation_count(&fs::read_to_string(log)?), 1);
    Ok(())
}

#[tokio::test]
async fn none_mode_skips_local_thumbnail_generation_without_removing_existing_assets()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("No Thumbnails (2024)");
    fs::create_dir_all(&movie_dir)?;
    fs::write(movie_dir.join("No.Thumbnails.2024.mkv"), b"video")?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;
    sqlx::query("UPDATE libraries SET media_strategy_json = ? WHERE id = ?")
        .bind(r#"{"images":{"thumbnailScrapingMode":"NONE"}}"#)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    fake_ffmpeg(&fake, &log, 0)?;
    let report = ThumbnailService::with_runner(database.clone(), fake, Duration::from_secs(5))
        .generate_library(library.id)
        .await?;

    assert_eq!(report.considered, 0);
    assert_eq!(report.skipped_policy, 1);
    assert!(!movie_dir.join("No.Thumbnails.2024-poster.jpg").exists());
    assert!(!movie_dir.join("No.Thumbnails.2024-thumbnail.jpg").exists());
    let image_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_images")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(image_count, 0);
    Ok(())
}

#[tokio::test]
async fn screenshot_first_replaces_scraper_poster_and_thumbnail_independently()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Screenshot First (2024)");
    fs::create_dir_all(&movie_dir)?;
    let media_path = movie_dir.join("Screenshot.First.2024.mkv");
    fs::write(&media_path, b"video")?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let poster_path = movie_dir.join("Screenshot.First.2024-poster.jpg");
    let thumbnail_path = movie_dir.join("Screenshot.First.2024-thumbnail.jpg");
    let scraper_bytes = b"\xFF\xD8scraper\xFF\xD9";
    fs::write(&poster_path, scraper_bytes)?;
    fs::write(&thumbnail_path, scraper_bytes)?;
    for (image_type, path) in [("POSTER", &poster_path), ("THUMB", &thumbnail_path)] {
        sqlx::query(
            "INSERT INTO item_images (
                id, item_id, image_type, image_index, local_path, file_size, content_tag, source
             ) VALUES (?, ?, ?, 0, ?, ?, 'scraper', 'TMDB')",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&item_id)
        .bind(image_type)
        .bind(path.to_string_lossy().as_ref())
        .bind(i64::try_from(scraper_bytes.len())?)
        .execute(database.pool())
        .await?;
    }
    sqlx::query("UPDATE libraries SET media_strategy_json = ? WHERE id = ?")
        .bind(r#"{"images":{"thumbnailScrapingMode":"SCREENSHOT_FIRST"}}"#)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    fake_ffmpeg(&fake, &log, 0)?;
    let report = ThumbnailService::with_runner(database.clone(), fake, Duration::from_secs(5))
        .generate_library(library.id)
        .await?;

    assert_eq!(report.generated, 1);
    assert_eq!(fs::read(&poster_path)?, b"\xFF\xD8lux-thumb\xFF\xD9");
    assert_eq!(fs::read(&thumbnail_path)?, b"\xFF\xD8lux-thumb\xFF\xD9");
    let image_sources: Vec<(String, String)> =
        sqlx::query_as("SELECT image_type, source FROM item_images ORDER BY image_type")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(
        image_sources,
        vec![
            ("POSTER".to_owned(), "FFMPEG".to_owned()),
            ("THUMB".to_owned(), "FFMPEG".to_owned()),
        ]
    );
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
    let target = movie_dir.join("Local.Movie.2024-thumbnail.jpg");
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
async fn existing_scraper_poster_prevents_poster_fallback_but_allows_thumbnail_fallback()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Scraped Movie (2024)");
    fs::create_dir_all(&movie_dir)?;
    fs::write(movie_dir.join("Scraped.Movie.2024.mkv"), b"video")?;
    let poster_path = movie_dir.join("Scraped.Movie.2024-poster.jpg");
    let poster_bytes = b"\xff\xd8scraper-poster\xff\xd9";
    fs::write(&poster_path, poster_bytes)?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    sqlx::query(
        "INSERT INTO item_images (
            id, item_id, image_type, image_index, local_path, file_size, content_tag, source
         ) VALUES (?, ?, 'POSTER', 0, ?, ?, 'scraper', 'TMDB')",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&item_id)
    .bind(poster_path.to_string_lossy().as_ref())
    .bind(i64::try_from(poster_bytes.len())?)
    .execute(database.pool())
    .await?;

    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    fs::write(&log, "")?;
    fake_ffmpeg(&fake, &log, 0)?;
    let thumbnails = ThumbnailService::with_runner(database.clone(), fake, Duration::from_secs(5));

    let report = thumbnails.generate_library(library.id).await?;

    assert_eq!(report.generated, 1);
    assert_eq!(fs::read(&poster_path)?, poster_bytes);
    let thumbnail_path = movie_dir.join("Scraped.Movie.2024-thumbnail.jpg");
    assert_eq!(fs::read(&thumbnail_path)?, b"\xff\xd8lux-thumb\xff\xd9");
    let ffmpeg_arguments = fs::read_to_string(log)?;
    assert!(!ffmpeg_arguments.contains("scale=600:900"));
    assert!(ffmpeg_arguments.contains("scale=1280:720"));
    Ok(())
}

#[tokio::test]
async fn existing_series_episode_thumbnail_is_preserved_while_poster_is_generated()
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

    let ffmpeg_arguments = fs::read_to_string(log)?;
    assert!(ffmpeg_arguments.contains("scale=600:900"));
    assert!(!ffmpeg_arguments.contains("scale=1280:720"));
    let canonical_season_dir = fs::canonicalize(&season_dir)?;
    assert_eq!(
        fs::read(canonical_season_dir.join("Example.Show.S01E01-poster.jpg"))?,
        b"\xff\xd8lux-thumb\xff\xd9"
    );
    let image_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT ii.image_type, ii.source
         FROM item_images ii
         JOIN media_items mi ON mi.id = ii.item_id
         WHERE mi.item_type = 'EPISODE'
         ORDER BY ii.image_type",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        image_rows,
        vec![
            ("POSTER".to_owned(), "FFMPEG".to_owned()),
            ("THUMB".to_owned(), "LOCAL".to_owned()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn generated_episode_thumbnail_never_claims_episode_fanart_path()
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
    let episode_path = season_dir.join("Example.Show.S01E01.mkv");
    let fanart_path = season_dir.join("Example.Show.S01E01-thumb.jpg");
    fs::write(&episode_path, b"video")?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    fs::write(&fanart_path, b"\xff\xd8existing-fanart\xff\xd9")?;

    let episode_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'EPISODE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    sqlx::query(
        "INSERT INTO item_images (
            id, item_id, image_type, image_index, local_path, file_size, content_tag, source
         ) VALUES (?, ?, 'FANART', 0, ?, ?, 'fanart', 'TMDB')",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&episode_id)
    .bind(fanart_path.to_string_lossy().as_ref())
    .bind(i64::try_from(b"\xff\xd8existing-fanart\xff\xd9".len())?)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO item_images (
            id, item_id, image_type, image_index, local_path, file_size, content_tag, source
         ) VALUES (?, ?, 'THUMB', 0, ?, ?, 'thumbnail', 'FFMPEG')",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&episode_id)
    .bind(fanart_path.to_string_lossy().as_ref())
    .bind(i64::try_from(b"\xff\xd8existing-fanart\xff\xd9".len())?)
    .execute(database.pool())
    .await?;

    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    fs::write(&log, "")?;
    fake_ffmpeg(&fake, &log, 0)?;
    let thumbnails = ThumbnailService::with_runner(database.clone(), fake, Duration::from_secs(5));

    let report = thumbnails.generate_library(library.id).await?;

    assert_eq!(
        report.generated, 1,
        "unexpected thumbnail report: {report:?}"
    );
    assert_eq!(fs::read(&fanart_path)?, b"\xff\xd8existing-fanart\xff\xd9");
    let poster_path = fs::canonicalize(&season_dir)?.join("Example.Show.S01E01-poster.jpg");
    assert_eq!(fs::read(&poster_path)?, b"\xff\xd8lux-thumb\xff\xd9");
    let thumbnail_path = fs::canonicalize(&season_dir)?.join("Example.Show.S01E01-thumbnail.jpg");
    assert_eq!(fs::read(&thumbnail_path)?, b"\xff\xd8lux-thumb\xff\xd9");
    let indexed_thumbnail: (String, String) = sqlx::query_as(
        "SELECT image_type, local_path
         FROM item_images
         WHERE item_id = ? AND image_type = 'THUMB' AND image_index = 0",
    )
    .bind(&episode_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexed_thumbnail.0, "THUMB");
    assert_eq!(indexed_thumbnail.1, thumbnail_path.to_string_lossy());
    Ok(())
}

#[tokio::test]
async fn generated_episode_thumbnail_skips_a_canonical_path_owned_by_fanart()
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
    let episode_path = season_dir.join("Example.Show.S01E01.mkv");
    let fanart_path = season_dir.join("Example.Show.S01E01-thumbnail.jpg");
    fs::write(&episode_path, b"video")?;
    fs::write(&fanart_path, b"\xff\xd8existing-fanart\xff\xd9")?;
    let fanart_path = fs::canonicalize(&fanart_path)?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;

    let episode_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'EPISODE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    sqlx::query(
        "INSERT INTO item_images (
            id, item_id, image_type, image_index, local_path, file_size, content_tag, source
         ) VALUES (?, ?, 'FANART', 0, ?, ?, 'fanart', 'TMDB')",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&episode_id)
    .bind(fanart_path.to_string_lossy().as_ref())
    .bind(i64::try_from(b"\xff\xd8existing-fanart\xff\xd9".len())?)
    .execute(database.pool())
    .await?;

    let fake = temp_dir.path().join("ffmpeg");
    let log = temp_dir.path().join("ffmpeg.log");
    fs::write(&log, "")?;
    fake_ffmpeg(&fake, &log, 0)?;
    let thumbnails = ThumbnailService::with_runner(database.clone(), fake, Duration::from_secs(5));

    let report = thumbnails.generate_library(library.id).await?;

    assert_eq!(
        report.generated, 1,
        "unexpected thumbnail report: {report:?}"
    );
    let poster_path = fanart_path.with_file_name("Example.Show.S01E01-poster.jpg");
    assert_eq!(fs::read(&poster_path)?, b"\xff\xd8lux-thumb\xff\xd9");
    let thumbnail_path = fanart_path.with_file_name("Example.Show.S01E01-thumbnail-1.jpg");
    assert_eq!(fs::read(&thumbnail_path)?, b"\xff\xd8lux-thumb\xff\xd9");
    let indexed_thumbnail: String = sqlx::query_scalar(
        "SELECT local_path
         FROM item_images
         WHERE item_id = ? AND image_type = 'THUMB' AND image_index = 0",
    )
    .bind(&episode_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexed_thumbnail, thumbnail_path.to_string_lossy());
    assert_eq!(fs::read(&fanart_path)?, b"\xff\xd8existing-fanart\xff\xd9");
    Ok(())
}

#[tokio::test]
async fn ffmpeg_failure_does_not_fail_the_completed_scan() -> Result<(), Box<dyn std::error::Error>>
{
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
    assert_eq!(status, "COMPLETED");
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

#[tokio::test]
async fn thumbnail_workers_run_ffmpeg_concurrently_with_a_global_bound()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    for index in 0..4 {
        let movie_dir = root.join(format!("Movie {index} (2024)"));
        fs::create_dir_all(&movie_dir)?;
        fs::write(movie_dir.join(format!("Movie.{index}.2024.mkv")), b"video")?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 test path")?)
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let scan = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&scan.id, 100, None).await?;

    let fake = temp_dir.path().join("ffmpeg");
    let script = r#"#!/bin/sh
set -eu
state_dir="$(dirname "$0")/ffmpeg-state"
mkdir -p "$state_dir"
while ! mkdir "$state_dir/lock" 2>/dev/null; do sleep 0.001; done
current=$(cat "$state_dir/current" 2>/dev/null || printf '0')
current=$((current + 1))
printf '%s' "$current" > "$state_dir/current"
maximum=$(cat "$state_dir/maximum" 2>/dev/null || printf '0')
if [ "$current" -gt "$maximum" ]; then printf '%s' "$current" > "$state_dir/maximum"; fi
rmdir "$state_dir/lock"
sleep 0.25
while ! mkdir "$state_dir/lock" 2>/dev/null; do sleep 0.001; done
current=$(cat "$state_dir/current")
printf '%s' "$((current - 1))" > "$state_dir/current"
rmdir "$state_dir/lock"
for argument in "$@"; do
  case "$argument" in
    *.jpg) printf '\377\330lux-thumb\377\331' > "$argument" ;;
  esac
done
"#;
    fs::write(&fake, script)?;
    let mut permissions = fs::metadata(&fake)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake, permissions)?;

    let thumbnails = ThumbnailService::with_runner(database, fake.clone(), Duration::from_secs(5));
    let report = thumbnails.generate_library(library.id).await?;
    assert_eq!(report.generated, 4);
    let maximum = fs::read_to_string(
        fake.parent()
            .ok_or("missing fake ffmpeg parent")?
            .join("ffmpeg-state/maximum"),
    )?
    .trim()
    .parse::<usize>()?;
    assert!(
        maximum >= 2,
        "expected overlapping ffmpeg processes, saw {maximum}"
    );
    assert!(
        maximum <= 4,
        "thumbnail ffmpeg peak exceeded the default bound"
    );
    Ok(())
}
