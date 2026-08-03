use std::time::Duration;

use luxd::{
    application::{
        libraries::LibraryService,
        probe::{FfprobeRunner, MediaProbeService},
        scanner::{ScanJobError, ScanJobService},
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[tokio::test]
async fn scan_job_persists_batches_resumes_and_cancels() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    for (title, year) in [("Alpha", 2020), ("Beta", 2021), ("Gamma", 2022)] {
        let directory = root.join(format!("{title} Movie ({year})"));
        tokio::fs::create_dir_all(&directory).await?;
        tokio::fs::write(
            directory.join(format!("{title}.Movie.{year}.mkv")),
            b"fixture",
        )
        .await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    assert_eq!(job.status, "PENDING");
    assert_eq!(job.total_count, 3);
    assert!(matches!(
        jobs.create_movie_scan_job(library.id).await,
        Err(ScanJobError::AlreadyActive(_))
    ));

    let first_batch = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(first_batch.status, "RUNNING");
    assert_eq!(first_batch.processed, 1);
    let persisted: (String, i64, Option<String>) =
        sqlx::query_as("SELECT status, processed_count, cursor FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(persisted.0, "RUNNING");
    assert_eq!(persisted.1, 1);
    assert!(persisted.2.is_some());

    let restarted_jobs = ScanJobService::new(database.clone());
    assert!(
        restarted_jobs
            .active_job_ids()
            .await?
            .iter()
            .any(|id| id == &job.id)
    );
    let second_batch = restarted_jobs.run_batch(&job.id, 1).await?;
    assert_eq!(second_batch.status, "RUNNING");
    assert_eq!(second_batch.processed, 1);
    let third_batch = restarted_jobs.run_batch(&job.id, 10).await?;
    assert_eq!(third_batch.status, "RUNNING");
    assert_eq!(third_batch.processed, 1);
    let completed = restarted_jobs.run_batch(&job.id, 10).await?;
    assert_eq!(completed.status, "COMPLETED");
    assert!(completed.completed);
    let final_status: (String, i64, Option<String>) =
        sqlx::query_as("SELECT status, processed_count, cursor FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(final_status, ("COMPLETED".to_owned(), 3, None));
    assert!(
        !restarted_jobs
            .active_job_ids()
            .await?
            .iter()
            .any(|id| id == &job.id)
    );
    let item_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_items")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(item_count, 3);
    let root_cursor: Option<String> =
        sqlx::query_scalar("SELECT scan_cursor FROM library_roots WHERE library_id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(root_cursor, None);
    let event_codes: Vec<String> = sqlx::query_scalar(
        "SELECT event_code FROM scan_job_events WHERE job_id = ? ORDER BY created_at, id",
    )
    .bind(&job.id)
    .fetch_all(database.pool())
    .await?;
    assert!(event_codes.iter().any(|code| code == "JOB_CREATED"));
    assert!(event_codes.iter().any(|code| code == "JOB_STARTED"));
    assert!(event_codes.iter().any(|code| code == "BATCH_COMPLETED"));
    assert!(event_codes.iter().any(|code| code == "JOB_COMPLETED"));

    let cancel_job = restarted_jobs.create_movie_scan_job(library.id).await?;
    restarted_jobs.cancel(&cancel_job.id).await?;
    let cancelled = restarted_jobs.run_batch(&cancel_job.id, 1).await?;
    assert_eq!(cancelled.status, "CANCELLED");
    assert!(cancelled.completed);
    let cancel_event: (String, String) = sqlx::query_as(
        "SELECT level, event_code FROM scan_job_events
         WHERE job_id = ? AND event_code = 'JOB_CANCELLED'",
    )
    .bind(&cancel_job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        cancel_event,
        ("INFO".to_owned(), "JOB_CANCELLED".to_owned())
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn completed_scan_runs_pending_ffprobe_before_worker_returns()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{fs, os::unix::fs::PermissionsExt};

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Probe Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Probe.Movie.2024.mp4"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let fake_ffprobe = temp_dir.path().join("fake-ffprobe");
    fs::write(
        &fake_ffprobe,
        r#"#!/bin/sh
printf '%s' '{"format":{"format_name":"mp4","duration":"30","bit_rate":"128000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264"}]}'
"#,
    )?;
    let mut permissions = fs::metadata(&fake_ffprobe)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ffprobe, permissions)?;

    let jobs = ScanJobService::new(database.clone());
    let probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(fake_ffprobe, Duration::from_secs(5)),
    );
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, Some(probe)).await?;

    let source: (String, i64, i64, String) = sqlx::query_as(
        "SELECT container, duration_ticks, bitrate, probe_status FROM media_sources",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        source,
        ("mp4".to_owned(), 300_000_000, 128_000, "READY".to_owned())
    );
    let event_codes: Vec<String> = sqlx::query_scalar(
        "SELECT event_code FROM scan_job_events WHERE job_id = ? ORDER BY created_at, id",
    )
    .bind(&job.id)
    .fetch_all(database.pool())
    .await?;
    assert!(event_codes.iter().any(|code| code == "PROBE_COMPLETED"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn scan_job_marks_inaccessible_root_unavailable_and_recovers_after_restore()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Recovery.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    assert_eq!(initial.total_count, 1);
    finish_scan(&jobs, &initial.id).await?;

    let mut permissions = tokio::fs::metadata(&root).await?.permissions();
    permissions.set_mode(0o000);
    tokio::fs::set_permissions(&root, permissions).await?;

    let unavailable = jobs.create_movie_scan_job(library.id).await?;
    assert_eq!(unavailable.total_count, 0);
    finish_scan(&jobs, &unavailable.id).await?;
    let root_available: i64 =
        sqlx::query_scalar("SELECT is_available FROM library_roots WHERE library_id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(root_available, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_items")
            .fetch_one(database.pool())
            .await?,
        1
    );

    let mut permissions = tokio::fs::metadata(&root).await?.permissions();
    permissions.set_mode(0o755);
    tokio::fs::set_permissions(&root, permissions).await?;

    let recovered = jobs.create_movie_scan_job(library.id).await?;
    assert_eq!(recovered.total_count, 1);
    finish_scan(&jobs, &recovered.id).await?;
    let recovered_available: i64 =
        sqlx::query_scalar("SELECT is_available FROM library_roots WHERE library_id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(recovered_available, 1);
    Ok(())
}

#[cfg(unix)]
async fn finish_scan(
    jobs: &ScanJobService,
    job_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..10 {
        if jobs.run_batch(job_id, 100).await?.completed {
            return Ok(());
        }
    }
    Err("scan did not complete within the test batch limit".into())
}
