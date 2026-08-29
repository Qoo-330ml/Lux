mod common;

use std::{sync::Arc, time::Duration};

use common::{TestScraper, TestScraperConfig};
use luxd::{
    application::{
        access::{AccessPrincipal, MediaAccessService},
        catalog::{CatalogFilter, CatalogService},
        libraries::LibraryService,
        nfo::LocalNfoMetadataStore,
        probe::{FfprobeRunner, MediaProbeService},
        reidentify::MetadataReidentifyService,
        scanner::{IncrementalScanChange, ScanJobError, ScanJobService},
        scraper::ScraperProvider,
        thumbnails::ThumbnailService,
        watch::ChangeKind,
        webhooks::WebhookService,
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
    let activity: (Option<String>, String) =
        sqlx::query_as("SELECT current_item, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(activity.0.as_deref(), Some("Alpha.Movie.2020.mkv"));
    assert_eq!(activity.1, "INDEXING");
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
    let completed_activity: (Option<String>, String) =
        sqlx::query_as("SELECT current_item, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(completed_activity, (None, "POSTPROCESSING".to_owned()));
    assert!(
        restarted_jobs
            .active_job_ids()
            .await?
            .iter()
            .any(|id| id == &job.id)
    );
    restarted_jobs.run_to_completion(&job.id, 10, None).await?;
    let final_status: (String, Option<i64>, String) =
        sqlx::query_as("SELECT status, finished_at, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(final_status.0, "COMPLETED");
    assert!(final_status.1.is_some());
    assert_eq!(final_status.2, "IDLE");
    assert!(
        !restarted_jobs
            .active_job_ids()
            .await?
            .iter()
            .any(|id| id == &job.id)
    );
    let item_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE item_type <> 'FOLDER'")
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
async fn series_reconciliation_batches_hierarchy_and_versions()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root = temp_dir.path().join("Shows");
    let season = root.join("Example Show (2024)").join("Season 01");
    tokio::fs::create_dir_all(&season).await?;
    for name in [
        "Example.Show.S01E01.1080p.mkv",
        "Example.Show.S01E01.2160p.mkv",
        "Example.Show.S01E02.mkv",
    ] {
        tokio::fs::write(season.join(name), b"episode").await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let hierarchy_counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT item_type, COUNT(*) FROM media_items
         WHERE library_id = ? GROUP BY item_type ORDER BY item_type",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        hierarchy_counts,
        vec![
            ("EPISODE".to_owned(), 2),
            ("SEASON".to_owned(), 1),
            ("SERIES".to_owned(), 1),
        ]
    );
    let source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources ms
         JOIN media_items mi ON mi.id = ms.item_id
         WHERE mi.library_id = ?",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(source_count, 3);
    let batch_details: String = sqlx::query_scalar(
        "SELECT details_json FROM scan_job_events
         WHERE job_id = ? AND event_code = 'BATCH_COMPLETED'
         ORDER BY created_at, id LIMIT 1",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert!(batch_details.contains("\"concurrency\":"));
    Ok(())
}

#[tokio::test]
async fn mixed_reconciliation_batches_known_media_and_keeps_unresolved()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Mixed", LibraryKind::Mixed, false)
        .await?;
    let root = temp_dir.path().join("Mixed");
    let movie_dir = root.join("Known Movie (2020)");
    let episode_dir = root.join("Known Show").join("Season 01");
    let unresolved_dir = root.join("Unclear");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::create_dir_all(&episode_dir).await?;
    tokio::fs::create_dir_all(&unresolved_dir).await?;
    tokio::fs::write(movie_dir.join("Known.Movie.2020.mkv"), b"movie").await?;
    tokio::fs::write(episode_dir.join("Known.Show.S01E01.mkv"), b"episode").await?;
    tokio::fs::write(unresolved_dir.join("Mystery File.mkv"), b"unknown").await?;
    tokio::fs::write(root.join("Known Show").join("tvshow.nfo"), "<tvshow />").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT item_type, COUNT(*) FROM media_items
         WHERE library_id = ? GROUP BY item_type ORDER BY item_type",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        counts,
        vec![
            ("EPISODE".to_owned(), 1),
            ("FOLDER".to_owned(), 2),
            ("MOVIE".to_owned(), 1),
            ("SEASON".to_owned(), 1),
            ("SERIES".to_owned(), 1),
            ("UNRESOLVED".to_owned(), 1),
        ]
    );
    let source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources ms
         JOIN media_items mi ON mi.id = ms.item_id
         WHERE mi.library_id = ?",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(source_count, 3);
    let batch_details: String = sqlx::query_scalar(
        "SELECT details_json FROM scan_job_events
         WHERE job_id = ? AND event_code = 'BATCH_COMPLETED'
         ORDER BY created_at, id LIMIT 1",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert!(batch_details.contains("\"concurrency\":"));
    Ok(())
}

#[tokio::test]
async fn series_reconciliation_updates_changed_episode_without_duplicate_source()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root = temp_dir.path().join("Shows");
    let episode = root
        .join("Example Show")
        .join("Season 01")
        .join("Example.Show.S01E01.mkv");
    if let Some(parent) = episode.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&episode, b"before").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let first = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first.id, 100, None).await?;
    tokio::fs::write(&episode, b"after with a different size").await?;

    let second = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&second.id, 100, None).await?;
    let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_sources")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(source_count, 1);
    let missing_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM filesystem_entries WHERE is_missing = 1")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(missing_count, 0);
    Ok(())
}

#[tokio::test]
async fn cancelling_after_indexing_completion_does_not_cancel_scan()
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
    tokio::fs::write(root.join("Cancel.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&job.id, 100).await?.completed {
            break;
        }
    }
    let index_status: (String, String) =
        sqlx::query_as("SELECT status, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        index_status,
        ("COMPLETED".to_owned(), "POSTPROCESSING".to_owned())
    );
    jobs.cancel(&job.id).await?;
    let post_cancel_status: (String, String) =
        sqlx::query_as("SELECT status, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        post_cancel_status,
        ("COMPLETED".to_owned(), "POSTPROCESSING".to_owned())
    );
    let target_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert!(target_count > 0);

    jobs.run_to_completion(&job.id, 100, None).await?;
    let final_target_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(final_target_count, 0);
    Ok(())
}

#[tokio::test]
async fn completed_scan_enqueues_new_media_once_for_webhook_destinations()
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
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let webhooks = WebhookService::new(database.clone(), config.config_dir.clone())?;
    let event_types = vec!["MEDIA_ADDED".to_owned(), "SCAN_COMPLETED".to_owned()];
    webhooks
        .create_destination(
            "Test destination",
            "https://example.com/lux-hook",
            true,
            false,
            &event_types,
            Some("webhook-test-secret-1234"),
        )
        .await?;
    let jobs = ScanJobService::new(database.clone()).with_webhooks(webhooks);
    let first = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first.id, 100, None).await?;

    let media_added: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_events WHERE event_type = 'MEDIA_ADDED'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(media_added, 1);
    let payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM notification_events WHERE event_type = 'MEDIA_ADDED'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&payload)?["addedCount"],
        1
    );
    let scan_completed_payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM notification_events WHERE event_type = 'SCAN_COMPLETED'",
    )
    .fetch_one(database.pool())
    .await?;
    let scan_completed_payload =
        serde_json::from_str::<serde_json::Value>(&scan_completed_payload)?;
    assert_eq!(scan_completed_payload["status"], "COMPLETED");
    assert_eq!(scan_completed_payload["processedCount"], 1);

    let second = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&second.id, 100, None).await?;
    let media_added_after_rescan: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_events WHERE event_type = 'MEDIA_ADDED'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(media_added_after_rescan, 1);
    Ok(())
}

#[tokio::test]
async fn unchanged_reconciliation_skips_index_targets() -> Result<(), Box<dyn std::error::Error>> {
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
    let stable_directory = root.join("Stable Movie (2024)");
    tokio::fs::create_dir_all(&stable_directory).await?;
    tokio::fs::write(stable_directory.join("Stable.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let first = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&first.id, 100).await?.completed {
            break;
        }
    }
    let first_target_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&first.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(first_target_count, 2);
    jobs.run_to_completion(&first.id, 100, None).await?;

    let second = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&second.id, 100).await?.completed {
            break;
        }
    }
    let second_target_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&second.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(second_target_count, 0);
    jobs.run_to_completion(&second.id, 100, None).await?;

    let movie_item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    sqlx::query("UPDATE media_items SET parent_id = NULL WHERE id = ?")
        .bind(&movie_item_id)
        .execute(database.pool())
        .await?;
    let third = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&third.id, 100, None).await?;
    let parent_after_unchanged_rescan: Option<String> =
        sqlx::query_scalar("SELECT parent_id FROM media_items WHERE id = ?")
            .bind(&movie_item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        parent_after_unchanged_rescan, None,
        "an unchanged reconciliation must not enter the index repair path"
    );
    Ok(())
}

#[tokio::test]
async fn reconciliation_persists_removed_media_and_sidecar_targets()
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
    let kept = root.join("Kept.Movie.2024.mkv");
    let kept_nfo = root.join("Kept.Movie.2024.nfo");
    let removed = root.join("Removed.Movie.2023.mkv");
    tokio::fs::write(&kept, b"kept").await?;
    tokio::fs::write(&kept_nfo, b"<movie />").await?;
    tokio::fs::write(&removed, b"removed").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let first = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first.id, 100, None).await?;
    tokio::fs::remove_file(&kept_nfo).await?;
    tokio::fs::remove_file(&removed).await?;

    let second = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&second.id, 100).await?.completed {
            break;
        }
    }
    let targets: Vec<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT target_type, target_id, change_kind, metadata_state, thumbnail_state
         FROM scan_job_targets WHERE job_id = ? ORDER BY target_type, target_id",
    )
    .bind(&second.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(targets.len(), 3);
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.0 == "ITEM" && target.2 == "SIDECAR")
            .count(),
        1
    );
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.0 == "SOURCE" && target.2 == "REMOVED")
            .count(),
        1
    );
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.0 == "ITEM" && target.2 == "REMOVED")
            .count(),
        1
    );
    assert!(targets.iter().all(|target| {
        if target.2 == "SIDECAR" {
            target.3 == "PENDING" && target.4 == "PENDING"
        } else {
            target.3 == "SKIPPED" && target.4 == "SKIPPED"
        }
    }));
    Ok(())
}

#[tokio::test]
async fn completed_scan_enqueues_media_removed_for_missing_files()
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
    let media = root.join("Alpha.Movie.2020.mkv");
    tokio::fs::write(&media, b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let webhooks = WebhookService::new(database.clone(), config.config_dir.clone())?;
    let event_types = vec!["MEDIA_REMOVED".to_owned()];
    webhooks
        .create_destination(
            "Removal receiver",
            "https://example.com/lux-hook",
            true,
            false,
            &event_types,
            Some("webhook-test-secret-1234"),
        )
        .await?;
    let jobs = ScanJobService::new(database.clone()).with_webhooks(webhooks);
    let first = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first.id, 100, None).await?;
    tokio::fs::remove_file(media).await?;

    let second = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&second.id, 100, None).await?;
    let removed_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_events WHERE event_type = 'MEDIA_REMOVED'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(removed_events, 1);
    let payload: String = sqlx::query_scalar(
        "SELECT payload_json FROM notification_events WHERE event_type = 'MEDIA_REMOVED'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&payload)?["removedCount"],
        1
    );
    Ok(())
}

#[tokio::test]
async fn reconciliation_persists_discovered_file_count_before_discovery_finishes()
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
    tokio::fs::create_dir(root.join("Nested.Movie.2022")).await?;
    tokio::fs::write(
        root.join("Nested.Movie.2022").join("Nested.Movie.2022.mkv"),
        b"fixture",
    )
    .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let first_discovery = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(first_discovery.status, "RUNNING");

    let discovered_count: i64 =
        sqlx::query_scalar("SELECT total_count FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(discovered_count, 2);

    let discovery_completed: i64 =
        sqlx::query_scalar("SELECT discovery_completed FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(discovery_completed, 0);
    Ok(())
}

#[tokio::test]
async fn cancelling_a_pending_scan_finishes_immediately_and_cleans_work()
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
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.cancel(&job.id).await?;

    let status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(status, "CANCELLED");
    let cancel_requested: i64 =
        sqlx::query_scalar("SELECT cancel_requested FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(cancel_requested, 1);
    let event_code: String = sqlx::query_scalar(
        "SELECT event_code FROM scan_job_events
         WHERE job_id = ? ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(event_code, "JOB_CANCELLED");
    Ok(())
}

#[tokio::test]
async fn deleted_library_scan_worker_exits_as_cancelled_without_touching_media_files()
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
    let media_file = root.join("Movie.2024.mkv");
    tokio::fs::write(&media_file, b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.prepare_library_deletion(library.id).await?;
    libraries.delete_library(library.id).await?;

    let report = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(report.status, "CANCELLED");
    assert!(report.completed);
    assert!(
        media_file.exists(),
        "library deletion must not delete media files"
    );
    Ok(())
}

#[tokio::test]
async fn active_full_scan_allows_incremental_scan_enqueue() -> Result<(), Box<dyn std::error::Error>>
{
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

    let jobs = ScanJobService::new(database.clone());
    let full_scan = jobs.create_movie_scan_job(library.id).await?;
    let incremental_scan = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: "New.Movie.2024.mkv".to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    assert_ne!(incremental_scan.id, full_scan.id);
    assert_eq!(incremental_scan.job_type, "INCREMENTAL_SCAN");

    jobs.run_to_completion(&incremental_scan.id, 100, None)
        .await?;
    jobs.run_to_completion(&full_scan.id, 100, None).await?;
    let active_incremental_scan = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: "Another.Movie.2024.mkv".to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    let error = jobs
        .create_movie_scan_job(library.id)
        .await
        .expect_err("an active incremental scan must exclude full index work");
    assert!(matches!(
        error,
        ScanJobError::AlreadyActive(id) if id == active_incremental_scan.id
    ));
    Ok(())
}

#[tokio::test]
async fn realtime_incremental_scan_preempts_running_full_scan()
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
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let scan_lock = Arc::new(Semaphore::new(1));
    let jobs = ScanJobService::new(database.clone()).with_scan_lock(scan_lock.clone());
    let full_scan = jobs.create_movie_scan_job(library.id).await?;
    let first_batch = jobs.run_batch(&full_scan.id, 1).await?;
    assert!(!first_batch.completed);
    let full_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&full_scan.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(full_status, "RUNNING");

    let held_permit = scan_lock.clone().acquire_owned().await?;
    let full_job_id = full_scan.id.clone();
    let full_jobs = jobs.clone();
    let full_worker =
        tokio::spawn(async move { full_jobs.run_to_completion(&full_job_id, 1, None).await });

    tokio::time::sleep(Duration::from_millis(20)).await;
    let incremental_scan = ScanJobService::new(database.clone())
        .with_scan_lock(scan_lock.clone())
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_record.id.to_string(),
                relative_path: "Realtime.Movie.2024.mkv".to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    let incremental_job_id = incremental_scan.id.clone();
    let incremental_jobs = ScanJobService::new(database.clone()).with_scan_lock(scan_lock);
    let incremental_worker = tokio::spawn(async move {
        incremental_jobs
            .run_to_completion(&incremental_job_id, 1, None)
            .await
    });

    drop(held_permit);
    tokio::time::timeout(Duration::from_secs(3), async {
        full_worker.await??;
        incremental_worker.await??;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .await??;

    let events: Vec<(String, String)> = sqlx::query_as(
        "SELECT job_id, event_code
         FROM scan_job_events
         WHERE job_id IN (?, ?)
           AND event_code IN ('JOB_STARTED', 'JOB_COMPLETED')
         ORDER BY created_at, id",
    )
    .bind(&full_scan.id)
    .bind(&incremental_scan.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(events.len(), 4);
    assert_eq!(events[0], (full_scan.id.clone(), "JOB_STARTED".to_owned()));
    assert_eq!(
        events[1],
        (incremental_scan.id.clone(), "JOB_STARTED".to_owned())
    );
    assert_eq!(events[2], (incremental_scan.id, "JOB_COMPLETED".to_owned()));
    assert_eq!(events[3], (full_scan.id, "JOB_COMPLETED".to_owned()));
    Ok(())
}

#[tokio::test]
async fn item_scan_only_reconciles_the_source_folder() -> Result<(), Box<dyn std::error::Error>> {
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
    let alpha = root.join("Alpha");
    let beta = root.join("Beta");
    tokio::fs::create_dir_all(&alpha).await?;
    tokio::fs::create_dir_all(&beta).await?;
    tokio::fs::write(alpha.join("Alpha.Movie.2020.mkv"), b"fixture").await?;
    tokio::fs::write(beta.join("Beta.Movie.2021.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;
    let alpha_item_id: String = sqlx::query_scalar(
        "SELECT ms.item_id
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path = 'Alpha/Alpha.Movie.2020.mkv'",
    )
    .fetch_one(database.pool())
    .await?;

    tokio::fs::write(alpha.join("Alpha.New.Movie.2022.mkv"), b"fixture").await?;
    tokio::fs::write(beta.join("Beta.New.Movie.2023.mkv"), b"fixture").await?;

    let item_scan = jobs.create_item_folder_scan_job(&alpha_item_id).await?;
    assert_eq!(item_scan.job_type, "INCREMENTAL_SCAN");
    let queued_path: String =
        sqlx::query_scalar("SELECT relative_path FROM scan_job_paths WHERE job_id = ?")
            .bind(&item_scan.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(queued_path, "Alpha");
    let auto_metadata_match: i64 =
        sqlx::query_scalar("SELECT auto_metadata_match FROM scan_jobs WHERE id = ?")
            .bind(&item_scan.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(auto_metadata_match, 0);

    jobs.run_to_completion(&item_scan.id, 100, None).await?;
    let alpha_new_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries WHERE relative_path = 'Alpha/Alpha.New.Movie.2022.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    let beta_new_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries WHERE relative_path = 'Beta/Beta.New.Movie.2023.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(alpha_new_entries, 1);
    assert_eq!(beta_new_entries, 0);
    Ok(())
}

#[tokio::test]
async fn failed_scan_retry_reuses_job_progress_and_pending_entries()
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
    for (title, year) in [("Alpha", 2020), ("Beta", 2021), ("Gamma", 2022)] {
        tokio::fs::write(root.join(format!("{title}.Movie.{year}.mkv")), b"fixture").await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_batch(&job.id, 100).await?;
    jobs.run_batch(&job.id, 1).await?;

    let before_failure: (i64, i64) = sqlx::query_as(
        "SELECT processed_count,
                (SELECT COUNT(*) FROM reconciliation_scan_entries WHERE job_id = scan_jobs.id)
         FROM scan_jobs WHERE id = ?",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(before_failure, (1, 2));
    sqlx::query(
        "UPDATE scan_jobs
         SET status = 'FAILED', error = 'simulated failure', finished_at = unixepoch()
         WHERE id = ?",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await?;

    let retried = jobs.retry(&job.id).await?;
    assert_eq!(retried.id, job.id);
    assert_eq!(retried.status, "PENDING");
    assert_eq!(retried.processed_count, 1);
    let pending_after_retry: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reconciliation_scan_entries WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(pending_after_retry, 2);

    jobs.run_to_completion(&job.id, 100, None).await?;
    let completed: (String, i64, i64) =
        sqlx::query_as("SELECT status, processed_count, total_count FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(completed, ("COMPLETED".to_owned(), 3, 3));
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
async fn reconciliation_streams_large_directory_discovery_in_bounded_chunks()
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
    for index in 0..1_025 {
        tokio::fs::write(
            root.join(format!("Movie {index:04}.Movie.2024.mkv")),
            b"fixture",
        )
        .await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let discovery = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(discovery.processed, 0);
    let pending_files: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = ? AND entry_type = 'FILE'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(pending_files, 1_025);
    let directory_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = ? AND entry_type = 'DIRECTORY'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(directory_entries, 0);
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
async fn failed_reconciliation_keeps_checkpoint_for_retry() -> Result<(), Box<dyn std::error::Error>>
{
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
    let valid_relative_path = "Valid.Movie.2024.mkv";
    tokio::fs::write(root.join(valid_relative_path), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let discovery = jobs.run_batch(&job.id, 100).await?;
    assert!(!discovery.completed);
    let invalid_absolute_path = temp_dir.path().join("Outside.Movie.2025.mkv");
    tokio::fs::write(&invalid_absolute_path, b"fixture").await?;
    let invalid_absolute_path = invalid_absolute_path
        .to_str()
        .ok_or("non-utf8 invalid path")?;
    sqlx::query(
        "INSERT INTO filesystem_entries (
             id, library_root_id, relative_path, entry_kind, size, modified_at,
             last_seen_generation
         ) VALUES (?, (SELECT id FROM library_roots WHERE library_id = ?), ?, 'FILE', 0, 0, ?)",
    )
    .bind("invalid-checkpoint-entry")
    .bind(library.id.to_string())
    .bind(invalid_absolute_path)
    .bind("old-generation")
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE reconciliation_scan_entries
         SET relative_path = ?
         WHERE job_id = ? AND entry_type = 'FILE'",
    )
    .bind(invalid_absolute_path)
    .bind(&job.id)
    .execute(database.pool())
    .await?;

    let error = jobs
        .run_batch(&job.id, 100)
        .await
        .expect_err("invalid persisted work must fail the scan job");
    assert!(matches!(error, ScanJobError::Scanner(_)));
    let failed_status: (String, Option<String>) =
        sqlx::query_as("SELECT status, error FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(failed_status.0, "FAILED");
    assert!(failed_status.1.is_some());
    let remaining_work: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM reconciliation_scan_entries WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_work, 1);

    let retried = jobs.retry(&job.id).await?;
    assert_eq!(retried.status, "PENDING");
    sqlx::query(
        "UPDATE reconciliation_scan_entries
         SET relative_path = ?
         WHERE job_id = ? AND entry_type = 'FILE'",
    )
    .bind(valid_relative_path)
    .bind(&job.id)
    .execute(database.pool())
    .await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    let completed_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(completed_status, "COMPLETED");
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
async fn incremental_series_scan_queues_episode_and_hierarchy_metadata()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library_with_scraper("Shows", LibraryKind::Series, false, Some("tmdb"), true)
        .await?;
    let root = temp_dir.path().join("Shows");
    let season = root.join("Example Show (2024)").join("Season 01");
    tokio::fs::create_dir_all(&season).await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;

    let tmdb = TestScraper::new(TestScraperConfig {
        timeout: Duration::from_millis(1),
        ..TestScraperConfig::default()
    })?;
    let metadata =
        MetadataReidentifyService::new(database.clone(), ScraperProvider::from_adapter(tmdb));

    for episode_number in [1, 2] {
        tokio::fs::write(
            season.join(format!("Example.Show.S01E{episode_number:02}.mkv")),
            b"episode",
        )
        .await?;
    }
    let incremental = jobs
        .enqueue_incremental_changes(
            library.id,
            [1, 2]
                .into_iter()
                .map(|episode_number| IncrementalScanChange {
                    root_id: root_record.id.to_string(),
                    relative_path: format!(
                        "Example Show (2024)/Season 01/Example.Show.S01E{episode_number:02}.mkv"
                    ),
                    kind: ChangeKind::Create,
                })
                .collect(),
        )
        .await?;
    jobs.run_to_completion_with_metadata(&incremental.id, 100, None, Some(metadata))
        .await?;

    let metadata_job: (String, String, i64) = sqlx::query_as(
        "SELECT id, mode, total_count FROM metadata_reidentify_jobs ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(metadata_job.1, "FILL_MISSING");
    assert_eq!(metadata_job.2, 4);

    let item_types: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT mi.item_type, mi.episode_number
         FROM metadata_reidentify_job_items ji
         JOIN media_items mi ON mi.id = ji.item_id
         WHERE ji.job_id = ?
         ORDER BY CASE mi.item_type WHEN 'SERIES' THEN 0 WHEN 'SEASON' THEN 1 ELSE 2 END,
                  mi.episode_number",
    )
    .bind(&metadata_job.0)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        item_types,
        vec![
            ("SERIES".to_owned(), None),
            ("SEASON".to_owned(), None),
            ("EPISODE".to_owned(), Some(1)),
            ("EPISODE".to_owned(), Some(2)),
        ]
    );
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

    let tmdb = TestScraper::new(TestScraperConfig {
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
    let metadata =
        MetadataReidentifyService::new(database.clone(), ScraperProvider::from_adapter(tmdb));

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
    let remaining_targets: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_targets, 0);
    let event_codes: Vec<String> = sqlx::query_scalar(
        "SELECT event_code FROM scan_job_events WHERE job_id = ? ORDER BY created_at, id",
    )
    .bind(&job.id)
    .fetch_all(database.pool())
    .await?;
    assert!(event_codes.iter().any(|code| code == "PROBE_COMPLETED"));
    let probe_details: String = sqlx::query_scalar(
        "SELECT details_json FROM scan_job_events
         WHERE job_id = ? AND event_code = 'PROBE_COMPLETED'
         ORDER BY created_at, id LIMIT 1",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert!(probe_details.contains("\"elapsedMs\":"));
    assert!(probe_details.contains("\"itemsPerSecond\":"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn scan_postprocessing_persists_the_current_stage_while_ffprobe_runs()
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
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Slow.Movie.2024.mp4"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let fake_ffprobe = temp_dir.path().join("fake-ffprobe");
    fs::write(
        &fake_ffprobe,
        r#"#!/bin/sh
sleep 1
printf '%s' '{"format":{"format_name":"mp4"},"streams":[]}'
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
    let worker = tokio::spawn({
        let jobs = jobs.clone();
        let job_id = job.id.clone();
        async move { jobs.run_to_completion(&job_id, 100, Some(probe)).await }
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let activity: (String, Option<String>) =
                sqlx::query_as("SELECT scan_phase, current_item FROM scan_jobs WHERE id = ?")
                    .bind(&job.id)
                    .fetch_one(database.pool())
                    .await?;
            if activity.0 == "POSTPROCESSING" && activity.1.as_deref() == Some("媒体探测") {
                break Ok::<(), sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    worker.await??;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn failed_postprocessing_targets_leave_scan_completed_and_retryable()
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
    let movie_dir = root.join("Retry Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Retry.Movie.2024.mp4"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let fake_ffprobe = temp_dir.path().join("fake-ffprobe");
    fs::write(&fake_ffprobe, "#!/bin/sh\nexit 1\n")?;
    let mut permissions = fs::metadata(&fake_ffprobe)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ffprobe, permissions)?;

    let jobs = ScanJobService::new(database.clone());
    let failing_probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(&fake_ffprobe, Duration::from_secs(5)),
    );
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, Some(failing_probe))
        .await?;

    let status: (String, String, Option<String>) =
        sqlx::query_as("SELECT status, scan_phase, error FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(status.0, "COMPLETED");
    assert_eq!(status.1, "IDLE");
    assert_eq!(status.2, None);
    let target_state: String = sqlx::query_scalar(
        "SELECT probe_state FROM scan_job_targets WHERE job_id = ? AND target_type = 'SOURCE'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(target_state, "FAILED");
    let postprocessing_failed_event: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_job_events
         WHERE job_id = ? AND event_code = 'POSTPROCESSING_FAILED'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(postprocessing_failed_event, 1);

    fs::write(
        &fake_ffprobe,
        "#!/bin/sh\nprintf '%s' '{\"format\":{\"format_name\":\"mp4\"},\"streams\":[]}'\n",
    )?;
    let retried = jobs.retry(&job.id).await?;
    assert_eq!(retried.id, job.id);
    assert_eq!(retried.status, "COMPLETED");
    assert_eq!(retried.scan_phase, "POSTPROCESSING");
    let succeeding_probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(fake_ffprobe, Duration::from_secs(5)),
    );
    jobs.run_to_completion(&job.id, 100, Some(succeeding_probe))
        .await?;
    let final_status: (String, String) =
        sqlx::query_as("SELECT status, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(final_status, ("COMPLETED".to_owned(), "IDLE".to_owned()));
    Ok(())
}

#[tokio::test]
async fn pending_postprocessing_targets_make_scan_retryable()
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
    tokio::fs::write(root.join("Pending.Movie.2024.mp4"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&job.id, 100).await?.completed {
            break;
        }
    }
    sqlx::query(
        "UPDATE filesystem_entries SET is_missing = 1
         WHERE relative_path = 'Pending.Movie.2024.mp4'",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE scan_job_targets
         SET probe_state = 'SKIPPED', metadata_state = 'PENDING', thumbnail_state = 'SKIPPED'
         WHERE job_id = ? AND target_type = 'ITEM'",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await?;

    jobs.run_to_completion(&job.id, 100, None).await?;
    let status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(status, "COMPLETED");
    let event_code: String = sqlx::query_scalar(
        "SELECT event_code FROM scan_job_events
         WHERE job_id = ? AND event_code = 'POSTPROCESSING_FAILED'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(event_code, "POSTPROCESSING_FAILED");
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn metadata_and_thumbnail_failures_are_persisted_per_target()
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
    let movie_dir = root.join("Broken Metadata Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Broken.Metadata.Movie.2024.mp4"), b"fixture").await?;
    tokio::fs::write(movie_dir.join("Broken.Metadata.Movie.2024.nfo"), b"<movie").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone())
        .with_nfo_store(LocalNfoMetadataStore::new(database.clone()));
    let thumbnails =
        ThumbnailService::with_runner(database.clone(), "false", Duration::from_secs(5));
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion_with_metadata_and_thumbnails(&job.id, 100, None, None, Some(thumbnails))
        .await?;

    let states: (String, String, String) = sqlx::query_as(
        "SELECT status, metadata_state, thumbnail_state
         FROM scan_jobs sj
         JOIN scan_job_targets t ON t.job_id = sj.id
         WHERE sj.id = ? AND t.target_type = 'ITEM'",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        states,
        (
            "COMPLETED".to_owned(),
            "FAILED".to_owned(),
            "FAILED".to_owned()
        )
    );
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
#[tokio::test]
async fn postprocessing_does_not_hold_the_shared_scan_lock()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{fs, os::unix::fs::PermissionsExt};

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
    tokio::fs::write(first_root.join("First.Movie.2024.mkv"), b"fixture").await?;
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

    let fake_ffprobe = temp_dir.path().join("slow-ffprobe");
    fs::write(
        &fake_ffprobe,
        r#"#!/bin/sh
sleep 1
printf '%s' '{"format":{"format_name":"mp4"},"streams":[]}'
"#,
    )?;
    let mut permissions = fs::metadata(&fake_ffprobe)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ffprobe, permissions)?;

    let scan_lock = Arc::new(Semaphore::new(1));
    let first_jobs = ScanJobService::new(database.clone()).with_scan_lock(scan_lock.clone());
    let second_jobs = ScanJobService::new(database.clone()).with_scan_lock(scan_lock);
    let first_job = first_jobs.create_movie_scan_job(first_library.id).await?;
    let second_job = second_jobs.create_movie_scan_job(second_library.id).await?;
    let first_job_id = first_job.id.clone();
    let first_probe = MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(fake_ffprobe, Duration::from_secs(5)),
    );
    let first_worker = tokio::spawn(async move {
        first_jobs
            .run_to_completion(&first_job_id, 100, Some(first_probe))
            .await
    });

    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let phase: String = sqlx::query_scalar("SELECT scan_phase FROM scan_jobs WHERE id = ?")
                .bind(&first_job.id)
                .fetch_one(database.pool())
                .await?;
            if phase == "POSTPROCESSING" {
                break Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;

    let second_job_id = second_job.id.clone();
    let second_worker = tokio::spawn(async move {
        second_jobs
            .run_to_completion(&second_job_id, 100, None)
            .await
    });
    tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            let status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
                .bind(&second_job.id)
                .fetch_one(database.pool())
                .await?;
            if status != "PENDING" {
                break Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;

    second_worker.await??;
    first_worker.await??;
    let first_phase: String = sqlx::query_scalar("SELECT scan_phase FROM scan_jobs WHERE id = ?")
        .bind(&first_job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(first_phase, "IDLE");
    Ok(())
}

#[cfg(unix)]
async fn finish_scan(
    jobs: &ScanJobService,
    job_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    jobs.run_to_completion(job_id, 100, None).await?;
    Ok(())
}
