use std::{sync::Arc, time::Duration};

use luxd::{
    application::{
        access::{AccessPrincipal, MediaAccessService},
        catalog::{CatalogFilter, CatalogService},
        libraries::LibraryService,
        probe::{FfprobeRunner, MediaProbeService},
        reidentify::MetadataReidentifyService,
        scanner::{IncrementalScanChange, ScanJobError, ScanJobService},
        tmdb::{TmdbClient, TmdbClientConfig},
        tmdb_plugin::TmdbProvider,
        watch::ChangeKind,
    },
    config::Config,
    domain::ids::UserId,
    library::LibraryKind,
    storage::Database,
};
use tokio::sync::Semaphore;

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
    assert_eq!(job.total_count, 0);
    assert!(matches!(
        jobs.create_movie_scan_job(library.id).await,
        Err(ScanJobError::AlreadyActive(_))
    ));

    let root_discovery = jobs.run_batch(&job.id, 100).await?;
    assert_eq!(root_discovery.processed, 0);
    let child_discovery = jobs.run_batch(&job.id, 100).await?;
    assert_eq!(child_discovery.processed, 0);
    let discovered_total: i64 =
        sqlx::query_scalar("SELECT total_count FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(discovered_total, 3);

    let first_batch = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(first_batch.status, "RUNNING");
    assert_eq!(first_batch.processed, 1);
    let visible_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
         WHERE has_available_source = 1 AND removed_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        visible_items, 1,
        "committed scan batches must be visible immediately"
    );
    let batch_details: String = sqlx::query_scalar(
        "SELECT details_json FROM scan_job_events
         WHERE job_id = ? AND event_code = 'BATCH_COMPLETED'
         ORDER BY created_at, id LIMIT 1",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert!(batch_details.contains("\"concurrency\":"));
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
    let final_status: (String, i64, Option<String>, Option<i64>) = sqlx::query_as(
        "SELECT status, processed_count, cursor, finished_at FROM scan_jobs WHERE id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(final_status.0, "COMPLETED");
    assert_eq!(final_status.1, 3);
    assert_eq!(final_status.2, None);
    assert!(final_status.3.is_some());
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
    let cancelled_work: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reconciliation_scan_entries WHERE job_id = ?")
            .bind(&cancel_job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(cancelled_work, 0);
    Ok(())
}

#[tokio::test]
async fn reconciliation_job_discovers_once_and_processes_a_persisted_snapshot()
-> Result<(), Box<dyn std::error::Error>> {
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
    tokio::fs::write(root.join("Alpha.Movie.2020.mkv"), b"fixture").await?;
    tokio::fs::write(root.join("Beta.Movie.2021.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    assert_eq!(job.total_count, 0, "job creation must not walk the root");

    let discovery = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(discovery.status, "RUNNING");
    assert_eq!(discovery.processed, 0);
    assert!(!discovery.completed);
    let discovered_total: i64 =
        sqlx::query_scalar("SELECT total_count FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(discovered_total, 2);

    tokio::fs::write(root.join("Gamma.Movie.2022.mkv"), b"late fixture").await?;
    jobs.run_to_completion(&job.id, 1, None).await?;

    let final_counts: (i64, i64) =
        sqlx::query_as("SELECT processed_count, total_count FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(final_counts, (2, 2));
    let indexed_paths: Vec<String> =
        sqlx::query_scalar("SELECT relative_path FROM filesystem_entries ORDER BY relative_path")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(
        indexed_paths,
        vec![
            "Alpha.Movie.2020.mkv".to_owned(),
            "Beta.Movie.2021.mkv".to_owned()
        ]
    );
    let remaining_work: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reconciliation_scan_entries WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_work, 0);
    Ok(())
}

#[tokio::test]
async fn reconciliation_hides_items_after_their_last_file_is_deleted()
-> Result<(), Box<dyn std::error::Error>> {
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
    let deleted_path = root.join("Deleted.Movie.2020.mkv");
    tokio::fs::write(&deleted_path, b"fixture").await?;
    tokio::fs::write(root.join("Kept.Movie.2021.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    tokio::fs::remove_file(&deleted_path).await?;
    tokio::fs::write(root.join("Added.Movie.2022.mkv"), b"fixture").await?;
    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&reconciliation.id, 100, None)
        .await?;

    let deleted_is_missing: i64 = sqlx::query_scalar(
        "SELECT is_missing FROM filesystem_entries WHERE relative_path = 'Deleted.Movie.2020.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(deleted_is_missing, 1);

    let catalog = CatalogService::new(database.clone(), MediaAccessService::new(database.clone()));
    let principal = AccessPrincipal::new(UserId::new(), true);
    let page = catalog
        .list_library_items_filtered(
            principal,
            &library.id.to_string(),
            &CatalogFilter::default(),
            0,
            100,
        )
        .await?;
    let titles = page
        .items
        .iter()
        .map(|item| item.title.as_str())
        .collect::<Vec<_>>();
    assert_eq!(page.total, 2);
    assert_eq!(titles, vec!["Added Movie", "Kept Movie"]);

    let unfiltered_page = catalog
        .list_library_items(principal, &library.id.to_string(), 0, 100)
        .await?;
    assert_eq!(unfiltered_page.total, 2);
    let deleted_item_id: String = sqlx::query_scalar(
        "SELECT ms.item_id
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path = 'Deleted.Movie.2020.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(
        catalog
            .find_item(principal, &deleted_item_id)
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn reconciliation_discovery_resumes_without_reading_completed_directories_again()
-> Result<(), Box<dyn std::error::Error>> {
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
    for (directory, filename) in [
        ("Alpha", "Alpha.Movie.2020.mkv"),
        ("Beta", "Beta.Movie.2021.mkv"),
    ] {
        tokio::fs::create_dir_all(root.join(directory)).await?;
        tokio::fs::write(root.join(directory).join(filename), b"fixture").await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let root_discovery = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(root_discovery.processed, 0);
    let queued_directories: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = ? AND entry_type = 'DIRECTORY'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(queued_directories, 2);

    let late_directory = root.join("Gamma");
    tokio::fs::create_dir_all(&late_directory).await?;
    tokio::fs::write(late_directory.join("Gamma.Movie.2022.mkv"), b"late fixture").await?;

    let restarted_jobs = ScanJobService::new(database.clone());
    restarted_jobs.run_to_completion(&job.id, 1, None).await?;

    let indexed_paths: Vec<String> =
        sqlx::query_scalar("SELECT relative_path FROM filesystem_entries ORDER BY relative_path")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(
        indexed_paths,
        vec![
            "Alpha/Alpha.Movie.2020.mkv".to_owned(),
            "Beta/Beta.Movie.2021.mkv".to_owned()
        ]
    );
    Ok(())
}

#[tokio::test]
async fn reconciliation_does_not_mark_files_missing_when_root_disappears_after_discovery()
-> Result<(), Box<dyn std::error::Error>> {
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
    tokio::fs::write(root.join("Keep.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    let discovery = jobs.run_batch(&reconciliation.id, 100).await?;
    assert_eq!(discovery.processed, 0);
    let discovered_total: i64 =
        sqlx::query_scalar("SELECT total_count FROM scan_jobs WHERE id = ?")
            .bind(&reconciliation.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(discovered_total, 1);

    tokio::fs::rename(&root, temp_dir.path().join("Movies-unmounted")).await?;
    jobs.run_to_completion(&reconciliation.id, 100, None)
        .await?;

    let entry_missing: i64 = sqlx::query_scalar("SELECT is_missing FROM filesystem_entries")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(entry_missing, 0);
    let root_available: i64 = sqlx::query_scalar("SELECT is_available FROM library_roots")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(root_available, 0);
    let remaining_work: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reconciliation_scan_entries WHERE job_id = ?")
            .bind(&reconciliation.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_work, 0);
    Ok(())
}

#[tokio::test]
async fn reconciliation_skips_prefetched_sibling_directories_after_one_directory_fails()
-> Result<(), Box<dyn std::error::Error>> {
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
    for (directory, filename) in [
        ("Alpha", "Alpha.Movie.2020.mkv"),
        ("Beta", "Beta.Movie.2021.mkv"),
    ] {
        tokio::fs::create_dir_all(root.join(directory)).await?;
        tokio::fs::write(root.join(directory).join(filename), b"fixture").await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    let reconciliation = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_batch(&reconciliation.id, 100).await?;
    tokio::fs::rename(root.join("Alpha"), temp_dir.path().join("Alpha-unmounted")).await?;
    jobs.run_to_completion(&reconciliation.id, 100, None)
        .await?;

    let missing_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM filesystem_entries WHERE is_missing = 1")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(missing_count, 0);
    let root_available: i64 = sqlx::query_scalar("SELECT is_available FROM library_roots")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(root_available, 0);
    Ok(())
}

#[tokio::test]
async fn incremental_scan_processes_only_queued_file() -> Result<(), Box<dyn std::error::Error>> {
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
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;
    let relative_path = "New.Movie.2024.mkv";
    tokio::fs::write(root.join(relative_path), b"fixture").await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: relative_path.to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    assert_eq!(job.job_type, "INCREMENTAL_SCAN");

    jobs.run_to_completion(&job.id, 100, None).await?;

    let item_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_items")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(item_count, 1);
    let queued_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_paths WHERE job_id = ? AND processed_at IS NULL",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(queued_count, 0);
    Ok(())
}

#[tokio::test]
async fn incremental_scan_only_queues_metadata_when_library_switch_is_enabled()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library_with_scraper("Movies", LibraryKind::Movie, false, Some("tmdb"), false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Existing.Movie.2023.mkv"), b"existing").await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    let tmdb = TmdbClient::new(TmdbClientConfig {
        base_url: "http://127.0.0.1:9".to_owned(),
        proxy_url: None,
        api_key: Some("test-token".to_owned()),
        read_access_token: None,
        timeout: Duration::from_millis(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;
    let metadata = MetadataReidentifyService::new(database.clone(), TmdbProvider::from(tmdb));

    let disabled_path = "Disabled.Movie.2024.mkv";
    tokio::fs::write(root.join(disabled_path), b"disabled").await?;
    let disabled_job = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: disabled_path.to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    jobs.run_to_completion_with_metadata(&disabled_job.id, 100, None, Some(metadata.clone()))
        .await?;
    let disabled_metadata_jobs: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metadata_reidentify_jobs")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(disabled_metadata_jobs, 0);

    libraries
        .update_settings(
            library.id,
            luxd::application::libraries::LibrarySettingsPatch {
                realtime_metadata_auto_match_enabled: Some(true),
                ..Default::default()
            },
        )
        .await?;
    let enabled_path = "Enabled.Movie.2025.mkv";
    tokio::fs::write(root.join(enabled_path), b"enabled").await?;
    let enabled_job = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: enabled_path.to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    jobs.run_to_completion_with_metadata(&enabled_job.id, 100, None, Some(metadata.clone()))
        .await?;

    let metadata_job: (String, i64) = sqlx::query_as(
        "SELECT mode, total_count FROM metadata_reidentify_jobs ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(metadata_job, ("FILL_MISSING".to_owned(), 1));

    let sidecar_path = "Enabled.Movie.2025.nfo";
    tokio::fs::write(root.join(sidecar_path), b"<movie />").await?;
    let sidecar_job = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: sidecar_path.to_owned(),
                kind: ChangeKind::Modify,
            }],
        )
        .await?;
    jobs.run_to_completion_with_metadata(&sidecar_job.id, 100, None, Some(metadata))
        .await?;
    let metadata_job_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metadata_reidentify_jobs")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(metadata_job_count, 1);
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
    assert_eq!(initial.total_count, 0);
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
    assert_eq!(recovered.total_count, 0);
    finish_scan(&jobs, &recovered.id).await?;
    let recovered_available: i64 =
        sqlx::query_scalar("SELECT is_available FROM library_roots WHERE library_id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(recovered_available, 1);
    Ok(())
}

#[tokio::test]
async fn scans_from_different_libraries_are_serialized() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let first_library = libraries
        .create_library("First Movies", LibraryKind::Movie, false)
        .await?;
    let second_library = libraries
        .create_library("Second Movies", LibraryKind::Movie, false)
        .await?;
    let first_root = temp_dir.path().join("first");
    let second_root = temp_dir.path().join("second");
    tokio::fs::create_dir_all(&first_root).await?;
    tokio::fs::create_dir_all(&second_root).await?;
    for index in 0..128 {
        tokio::fs::write(
            first_root.join(format!("First.Movie.{}.mkv", 2000 + index)),
            b"fixture",
        )
        .await?;
    }
    tokio::fs::write(second_root.join("Second.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(
            first_library.id,
            first_root.to_str().ok_or("non-utf8 first root")?,
        )
        .await?;
    libraries
        .add_root(
            second_library.id,
            second_root.to_str().ok_or("non-utf8 second root")?,
        )
        .await?;

    let scan_lock = Arc::new(Semaphore::new(1));
    let held_permit = scan_lock.clone().acquire_owned().await?;
    let first_jobs = ScanJobService::new(database.clone()).with_scan_lock(scan_lock.clone());
    let second_jobs = ScanJobService::new(database.clone()).with_scan_lock(scan_lock.clone());
    let first_job = first_jobs.create_movie_scan_job(first_library.id).await?;
    let second_job = second_jobs.create_movie_scan_job(second_library.id).await?;
    let first_job_id = first_job.id.clone();
    let second_job_id = second_job.id.clone();

    let first_worker =
        tokio::spawn(async move { first_jobs.run_to_completion(&first_job_id, 50, None).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let second_worker = tokio::spawn(async move {
        second_jobs
            .run_to_completion(&second_job_id, 50, None)
            .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let first_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&first_job.id)
        .fetch_one(database.pool())
        .await?;
    let second_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&second_job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(first_status, "PENDING");
    assert_eq!(second_status, "PENDING");

    drop(held_permit);
    first_worker.await??;
    second_worker.await??;

    let events: Vec<(String, String)> = sqlx::query_as(
        "SELECT job_id, event_code
         FROM scan_job_events
         WHERE job_id IN (?, ?)
           AND event_code IN ('JOB_STARTED', 'JOB_COMPLETED')
         ORDER BY created_at, id",
    )
    .bind(&first_job.id)
    .bind(&second_job.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(events.len(), 4);
    assert_eq!(events[1].0, events[0].0);
    assert_eq!(events[1].1, "JOB_COMPLETED");
    assert_ne!(events[2].0, events[0].0);
    assert_eq!(events[2].1, "JOB_STARTED");
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
