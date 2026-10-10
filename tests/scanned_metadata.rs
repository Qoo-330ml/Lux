use luxd::{
    application::scanner::LibraryScanner,
    application::{
        admin_events::{UserEventHub, UserEventScope},
        libraries::LibraryService,
        scanner::{IncrementalScanChange, ScanJobService},
        watch::ChangeKind,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};

async fn wait_for_local_metadata_batches(
    database: &Database,
    job_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let (pending, failed): (i64, i64) = sqlx::query_as(
                "SELECT COUNT(*) FILTER (WHERE status IN ('PENDING', 'RUNNING')),
                        COUNT(*) FILTER (WHERE status = 'FAILED')
                 FROM scan_local_metadata_batches WHERE job_id = ?",
            )
            .bind(job_id)
            .fetch_one(database.pool())
            .await?;
            if failed > 0 {
                return Err(sqlx::Error::Protocol(format!(
                    "{failed} local metadata batch(es) failed"
                )));
            }
            if pending == 0 {
                return Ok(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn movie_variant_rescan_preserves_nfo_identity_and_filename_poster()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("ABS-123");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("ABS-123.mp4"), b"base").await?;
    tokio::fs::write(
        movie_dir.join("ABS-123.nfo"),
        "<movie><title>Synthetic NFO Title</title><year>2024</year></movie>",
    )
    .await?;
    tokio::fs::write(movie_dir.join("ABS-123-poster.jpg"), b"poster").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-UTF8 root")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(library.id).await?;
    tokio::fs::write(movie_dir.join("ABS-123-C.mp4"), b"variant").await?;
    scanner.scan_movie_library(library.id).await?;
    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 1, None).await?;
    wait_for_local_metadata_batches(&database, &initial.id).await?;

    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items
         WHERE library_id = ? AND item_type = 'MOVIE' AND removed_at IS NULL",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let expected = vec![(
        item_id.clone(),
        "Synthetic NFO Title".to_owned(),
        Some(2024_i64),
        2_i64,
        1_i64,
    )];
    let read_movies = || async {
        sqlx::query_as::<_, (String, String, Option<i64>, i64, i64)>(
            "SELECT item.id, item.title, item.production_year,
                    (SELECT COUNT(*) FROM media_sources WHERE item_id = item.id),
                    (SELECT COUNT(*) FROM item_images WHERE item_id = item.id
                     AND image_type = 'POSTER' AND local_path LIKE '%/ABS-123-poster.jpg')
             FROM media_items item
             WHERE item.library_id = ? AND item.item_type = 'MOVIE'
               AND item.removed_at IS NULL AND item.merged_into_item_id IS NULL
             ORDER BY item.id",
        )
        .bind(library.id.to_string())
        .fetch_all(database.pool())
        .await
    };
    assert_eq!(read_movies().await?, expected, "initial NFO/image import");
    let source_ids: Vec<String> = sqlx::query_scalar("SELECT id FROM media_sources ORDER BY id")
        .fetch_all(database.pool())
        .await?;
    sqlx::query("UPDATE media_sources SET probe_status = 'READY'")
        .execute(database.pool())
        .await?;

    for _ in 0..2 {
        let rescan = jobs.create_movie_scan_job(library.id).await?;
        jobs.run_to_completion(&rescan.id, 1, None).await?;
        wait_for_local_metadata_batches(&database, &rescan.id).await?;
        assert_eq!(read_movies().await?, expected, "unchanged full rescan");
    }
    let probe_statuses: Vec<String> = sqlx::query_scalar("SELECT probe_status FROM media_sources")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(probe_statuses, vec!["READY".to_owned(); 2]);

    tokio::fs::write(movie_dir.join("ABS-123-C.mp4"), b"changed variant").await?;
    let changed = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&changed.id, 1, None).await?;
    wait_for_local_metadata_batches(&database, &changed.id).await?;
    assert_eq!(read_movies().await?, expected, "changed full rescan");

    LibraryScanner::new(database.clone())
        .scan_movie_directory(library.id, &movie_dir)
        .await?;
    assert_eq!(read_movies().await?, expected, "directory rescan");
    let rescanned_source_ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM media_sources ORDER BY id")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(rescanned_source_ids, source_ids);

    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, original_title,
             identification_status, has_available_source
         ) VALUES ('legacy-split-variant', ?, 'MOVIE', 'ABS 123 C', 'abs 123 c',
                   'ABS 123 C', 'LOCAL_CONFIRMED', 1)",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE media_sources SET item_id = 'legacy-split-variant', edition_name = NULL
         WHERE filesystem_entry_id = (
             SELECT id FROM filesystem_entries WHERE relative_path = 'ABS-123/ABS-123-C.mp4'
         )",
    )
    .execute(database.pool())
    .await?;
    let repair = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&repair.id, 1, None).await?;
    wait_for_local_metadata_batches(&database, &repair.id).await?;
    assert_eq!(
        read_movies().await?,
        expected,
        "repair an already split version"
    );
    let repaired_edition: Option<String> = sqlx::query_scalar(
        "SELECT edition_name FROM media_sources WHERE filesystem_entry_id = (
             SELECT id FROM filesystem_entries WHERE relative_path = 'ABS-123/ABS-123-C.mp4'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(repaired_edition.as_deref(), Some("C"));
    Ok(())
}

#[tokio::test]
async fn local_metadata_worker_backfills_posters_for_unchanged_indexed_items()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Existing Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Existing.Movie.2024.mkv"), b"movie").await?;
    tokio::fs::write(
        movie_dir.join("Existing.Movie.2024.nfo"),
        "<movie><title>Existing Title From NFO</title></movie>",
    )
    .await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"poster").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?
        .root;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id = sqlx::query_scalar::<_, String>(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM scan_local_metadata_batches")
            .fetch_one(database.pool())
            .await?,
        0,
        "this existing item has no newly created scan outbox work"
    );
    assert_ne!(
        sqlx::query_scalar::<_, String>("SELECT title FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?,
        "Existing Title From NFO",
        "the pre-existing item has not had its NFO processed yet"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND image_type = 'POSTER'",
        )
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?,
        0,
        "the pre-existing poster has not been indexed yet"
    );
    sqlx::query(
        "INSERT INTO scan_local_metadata_backfills (library_root_id, status, attempts)
         VALUES (?, 'RUNNING', 1)",
    )
    .bind(root.id.to_string())
    .execute(database.pool())
    .await?;

    let user_events = UserEventHub::new();
    let mut user_event_receiver = user_events.subscribe();
    let jobs = ScanJobService::new(database.clone()).with_user_events(user_events);
    jobs.start_local_metadata_outbox_worker().await?;
    let registered_roots: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_local_metadata_backfills WHERE library_root_id =
            (SELECT id FROM library_roots WHERE library_id = ?)",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        registered_roots, 1,
        "worker startup registers existing roots"
    );

    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let image_count: i64 = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM item_images
                 WHERE item_id = ? AND image_type = 'POSTER'
                   AND local_path LIKE '%/poster.jpg'",
            )
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
            if image_count > 0 {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert_eq!(
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            user_event_receiver.recv(),
        )
        .await??,
        UserEventScope::Home
    );

    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let image_count: i64 = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM item_images
                 WHERE item_id = ? AND image_type = 'POSTER'
                   AND local_path LIKE '%/poster.jpg'",
            )
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
            let title: String =
                sqlx::query_scalar::<_, String>("SELECT title FROM media_items WHERE id = ?")
                    .bind(&item_id)
                    .fetch_one(database.pool())
                    .await?;
            let backfill_status: String = sqlx::query_scalar::<_, String>(
                "SELECT status FROM scan_local_metadata_backfills
                 WHERE library_root_id = (SELECT id FROM library_roots WHERE library_id = ?)",
            )
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
            if image_count == 1
                && title == "Existing Title From NFO"
                && backfill_status == "COMPLETED"
            {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT attempts FROM scan_local_metadata_backfills WHERE library_root_id = ?",
        )
        .bind(root.id.to_string())
        .fetch_one(database.pool())
        .await?,
        2,
        "startup requeues and resumes an interrupted backfill page"
    );

    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM metadata_reidentify_jobs WHERE mode = 'FILL_MISSING'",
        )
        .fetch_one(database.pool())
        .await?,
        0,
        "local backfill does not call online metadata providers"
    );
    Ok(())
}

#[tokio::test]
async fn local_metadata_backfill_retries_nfo_failure_without_advancing_its_cursor()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Retry Existing Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Retry.Existing.Movie.2024.mkv"), b"movie").await?;
    tokio::fs::write(
        movie_dir.join("Retry.Existing.Movie.2024.nfo"),
        "<movie><title>Recovered Title From NFO</title></movie>",
    )
    .await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"poster").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?
        .root;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id = sqlx::query_scalar::<_, String>(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    sqlx::query(
        "CREATE TRIGGER fail_backfill_nfo_update
         BEFORE UPDATE OF title ON media_items
         WHEN OLD.item_type = 'MOVIE'
         BEGIN SELECT RAISE(ABORT, 'injected backfill NFO failure'); END",
    )
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone());
    jobs.start_local_metadata_outbox_worker().await?;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status: String = sqlx::query_scalar::<_, String>(
                "SELECT status FROM scan_local_metadata_backfills WHERE library_root_id = ?",
            )
            .bind(root.id.to_string())
            .fetch_one(database.pool())
            .await?;
            if status == "FAILED" {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;

    let failed_state: (Option<String>, i64) = sqlx::query_as(
        "SELECT cursor_entry_id, attempts FROM scan_local_metadata_backfills
         WHERE library_root_id = ?",
    )
    .bind(root.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        failed_state.0, None,
        "failed NFO does not advance the page cursor"
    );
    assert_eq!(failed_state.1, 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND image_type = 'POSTER'",
        )
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?,
        1,
        "the poster is indexed before the failing NFO stage"
    );

    sqlx::query("DROP TRIGGER fail_backfill_nfo_update")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "UPDATE scan_local_metadata_backfills SET next_attempt_at = 0
         WHERE library_root_id = ?",
    )
    .bind(root.id.to_string())
    .execute(database.pool())
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let (status, title, attempts): (String, String, i64) = sqlx::query_as(
                "SELECT backfill.status, item.title, backfill.attempts
                 FROM scan_local_metadata_backfills backfill
                 JOIN library_roots root ON root.id = backfill.library_root_id
                 JOIN media_items item ON item.library_id = root.library_id
                 WHERE root.id = ? AND item.id = ?",
            )
            .bind(root.id.to_string())
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
            if status == "COMPLETED" && title == "Recovered Title From NFO" && attempts == 2 {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn local_metadata_backfill_conflict_advances_without_retry()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    for (stem, content) in [
        ("Backfill.Target.Movie.1987", "movie-a"),
        ("Backfill.Different.Movie.2018", "movie-b"),
    ] {
        tokio::fs::write(media_root.join(format!("{stem}.mkv")), content).await?;
        tokio::fs::write(
            media_root.join(format!("{stem}.nfo")),
            "<movie><title>Backfill Target Movie</title><year>1987</year></movie>",
        )
        .await?;
    }

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?
        .root;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    sqlx::query(
        "INSERT INTO scan_local_metadata_backfills (library_root_id, status, attempts)
         VALUES (?, 'RUNNING', 1)",
    )
    .bind(root.id.to_string())
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone());
    jobs.start_local_metadata_outbox_worker().await?;
    let backfill: (String, i64, Option<String>) =
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let row: (String, i64, Option<String>) = sqlx::query_as(
                    "SELECT status, attempts, error
                     FROM scan_local_metadata_backfills WHERE library_root_id = ?",
                )
                .bind(root.id.to_string())
                .fetch_one(database.pool())
                .await?;
                if !matches!(row.0.as_str(), "PENDING" | "RUNNING") {
                    return Ok::<_, sqlx::Error>(row);
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await??;
    assert_eq!(backfill.0, "COMPLETED");
    assert_eq!(backfill.1, 2);
    assert!(
        backfill
            .2
            .as_deref()
            .is_some_and(|error| error.contains("non-retryable"))
    );
    Ok(())
}

#[tokio::test]
async fn local_metadata_backfill_retry_keeps_conflicted_item_excluded()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    for (directory, file, nfo) in [
        (
            "00 Backfill Primary (1987)",
            "Backfill.Primary.1987.mkv",
            "<movie><title>Shared Backfill Conflict</title><year>1987</year></movie>",
        ),
        (
            "01 Backfill Duplicate (2018)",
            "Backfill.Duplicate.2018.mkv",
            "<movie><title>Shared Backfill Conflict</title><year>1987</year></movie>",
        ),
        (
            "02 Backfill Retry Target (2024)",
            "Backfill.Retry.Target.2024.mkv",
            "<movie><title>Backfill Retry Target From NFO</title><year>2024</year></movie>",
        ),
    ] {
        let movie_dir = media_root.join(directory);
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(movie_dir.join(file), b"movie").await?;
        tokio::fs::write(movie_dir.join("movie.nfo"), nfo).await?;
    }

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?
        .root;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let retry_item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items
         WHERE library_id = ? AND item_type = 'MOVIE' AND production_year = 2024",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    sqlx::query(
        "CREATE TRIGGER fail_backfill_retry_target_nfo_update
         BEFORE UPDATE OF title ON media_items
         WHEN OLD.item_type = 'MOVIE' AND OLD.production_year = 2024
         BEGIN SELECT RAISE(ABORT, 'injected transient backfill NFO failure'); END",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_local_metadata_backfills (library_root_id, status, attempts)
         VALUES (?, 'RUNNING', 1)",
    )
    .bind(root.id.to_string())
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone());
    jobs.start_local_metadata_outbox_worker().await?;
    let failed_page: (i64, Option<String>, String) =
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let page: (String, i64, Option<String>, String) = sqlx::query_as(
                    "SELECT status, attempts, error, non_retryable_item_ids_json
                     FROM scan_local_metadata_backfills WHERE library_root_id = ?",
                )
                .bind(root.id.to_string())
                .fetch_one(database.pool())
                .await?;
                if page.0 == "FAILED" {
                    return Ok::<_, sqlx::Error>((page.1, page.2, page.3));
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await??;
    assert_eq!(failed_page.0, 2);
    let conflict_item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items
         WHERE library_id = ? AND item_type = 'MOVIE'
           AND title <> 'Shared Backfill Conflict' AND production_year IN (1987, 2018)",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let excluded_item_ids: Vec<String> = serde_json::from_str(&failed_page.2)?;
    assert!(
        excluded_item_ids.contains(&conflict_item_id),
        "the conflicted item must be persisted as excluded before retrying the page"
    );

    sqlx::query("DROP TRIGGER fail_backfill_retry_target_nfo_update")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "UPDATE scan_local_metadata_backfills SET next_attempt_at = 0
         WHERE library_root_id = ?",
    )
    .bind(root.id.to_string())
    .execute(database.pool())
    .await?;
    let completed_page: (String, i64, Option<String>) =
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let page: (String, i64, Option<String>) = sqlx::query_as(
                    "SELECT status, attempts, error FROM scan_local_metadata_backfills
                     WHERE library_root_id = ?",
                )
                .bind(root.id.to_string())
                .fetch_one(database.pool())
                .await?;
                if page.0 == "COMPLETED" {
                    return Ok::<_, sqlx::Error>(page);
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await??;
    assert_eq!(completed_page.1, 3);
    assert!(
        completed_page
            .2
            .as_deref()
            .is_some_and(|error| error.contains("non-retryable"))
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT title FROM media_items WHERE id = ?")
            .bind(&retry_item_id)
            .fetch_one(database.pool())
            .await?,
        "Backfill Retry Target From NFO"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn manifest_scan_indexes_poster_while_local_nfo_is_blocked()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt, time::Duration};

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let first_movie_dir = media_root.join("00 Slow NFO (2024)");
    tokio::fs::create_dir_all(&first_movie_dir).await?;
    tokio::fs::write(first_movie_dir.join("00.Slow.NFO.2024.mkv"), b"movie").await?;
    tokio::fs::write(first_movie_dir.join("poster.jpg"), b"poster").await?;
    let nfo_path = first_movie_dir.join("00.Slow.NFO.2024.nfo");
    let nfo_path_c = CString::new(nfo_path.as_os_str().as_bytes())?;
    // SAFETY: the path is a valid NUL-terminated filesystem path and mode is restrictive.
    let mkfifo_result = unsafe { libc::mkfifo(nfo_path_c.as_ptr(), 0o600) };
    assert_eq!(
        mkfifo_result, 0,
        "failed to create the blocking NFO fixture"
    );

    let second_movie_dir = media_root.join("01 Fast NFO (2024)");
    tokio::fs::create_dir_all(&second_movie_dir).await?;
    tokio::fs::write(second_movie_dir.join("01.Fast.NFO.2024.mkv"), b"movie").await?;
    tokio::fs::write(second_movie_dir.join("poster.jpg"), b"poster").await?;

    for index in 0..1_000 {
        let directory = media_root.join(format!("Later Movie {index:04} (2024)"));
        tokio::fs::create_dir_all(&directory).await?;
        tokio::fs::write(
            directory.join(format!("Later.Movie.{index:04}.2024.mkv")),
            b"movie",
        )
        .await?;
    }

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    sqlx::query("CREATE TABLE test_local_metadata_scan_job (job_id TEXT NOT NULL)")
        .execute(database.pool())
        .await?;
    sqlx::query("CREATE TABLE test_local_metadata_claim_states (state TEXT NOT NULL)")
        .execute(database.pool())
        .await?;
    sqlx::query("INSERT INTO test_local_metadata_scan_job (job_id) VALUES (?)")
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "CREATE TRIGGER capture_local_metadata_claim_state
         AFTER UPDATE OF status ON scan_local_metadata_batches
         WHEN NEW.status = 'RUNNING'
           AND NEW.job_id = (SELECT job_id FROM test_local_metadata_scan_job LIMIT 1)
         BEGIN
             INSERT INTO test_local_metadata_claim_states (state)
             SELECT state FROM scan_manifests WHERE job_id = NEW.job_id;
         END",
    )
    .execute(database.pool())
    .await?;
    let job_id = job.id.clone();
    let scan_jobs = jobs.clone();
    let mut scan = tokio::spawn(async move { scan_jobs.run_to_completion(&job_id, 1, None).await });

    let poster_visible_before_nfo = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let poster_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM item_images
                 WHERE item_images.local_path LIKE '%01 Fast NFO (2024)/poster.jpg'
                   AND item_images.image_type = 'POSTER'",
            )
            .fetch_one(database.pool())
            .await?;
            let local_batch_running: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM scan_local_metadata_batches
                 WHERE job_id = ? AND status = 'RUNNING'",
            )
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
            if poster_count > 0 && local_batch_running > 0 {
                return Ok::<_, sqlx::Error>(true);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or(Ok(false))?;

    let queue_state: Vec<(String, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*) FROM scan_local_metadata_batches
         WHERE job_id = ? GROUP BY status ORDER BY status",
    )
    .bind(&job.id)
    .fetch_all(database.pool())
    .await?;
    let scan_state: (String, String) =
        sqlx::query_as("SELECT status, scan_phase FROM scan_jobs WHERE id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    let image_paths: Vec<(String, String)> = sqlx::query_as(
        "SELECT item_images.image_type, item_images.local_path FROM item_images
         JOIN media_items ON media_items.id = item_images.item_id
         ORDER BY media_items.sort_title LIMIT 10",
    )
    .fetch_all(database.pool())
    .await?;
    let early_claim_states: Vec<String> =
        sqlx::query_scalar("SELECT state FROM test_local_metadata_claim_states")
            .fetch_all(database.pool())
            .await?;
    let scan_finished_before_nfo_unblocked =
        tokio::time::timeout(Duration::from_secs(30), &mut scan)
            .await
            .is_ok();

    // Release the NFO read whether the assertion passes or fails, so the worker can exit.
    tokio::fs::write(&nfo_path, b"<movie><title>Slow NFO</title></movie>").await?;
    if !scan_finished_before_nfo_unblocked {
        scan.await??;
    }
    wait_for_local_metadata_batches(&database, &job.id).await?;

    assert!(
        poster_visible_before_nfo,
        "a later poster should be indexed while the first NFO is blocked; scan={scan_state:?}, queue={queue_state:?}, images={image_paths:?}"
    );
    assert!(
        early_claim_states
            .iter()
            .any(|state| state == "DISCOVERING"),
        "the outbox worker should claim a batch during discovery; states={early_claim_states:?}"
    );
    assert!(
        scan_finished_before_nfo_unblocked,
        "scan completion should not wait for local NFO processing; scan={scan_state:?}, queue={queue_state:?}"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn blocked_first_local_poster_does_not_block_manifest_scan()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{
        ffi::CString,
        io::Write,
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
        time::Duration,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let first_movie_dir = media_root.join("00 Blocked Poster (2024)");
    let second_movie_dir = media_root.join("01 Later Poster (2024)");
    tokio::fs::create_dir_all(&first_movie_dir).await?;
    tokio::fs::create_dir_all(&second_movie_dir).await?;
    tokio::fs::write(first_movie_dir.join("00.Blocked.Poster.2024.mkv"), b"movie").await?;
    tokio::fs::write(second_movie_dir.join("01.Later.Poster.2024.mkv"), b"movie").await?;
    let blocked_poster = first_movie_dir.join("poster.png");
    let blocked_poster_c = CString::new(blocked_poster.as_os_str().as_bytes())?;
    // SAFETY: the path is a valid NUL-terminated fixture path and mode is restrictive.
    let mkfifo_result = unsafe { libc::mkfifo(blocked_poster_c.as_ptr(), 0o600) };
    assert_eq!(mkfifo_result, 0, "failed to create blocked poster fixture");

    let mut poster_png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([12, 34, 56, 255]),
    ))
    .write_to(&mut poster_png, image::ImageFormat::Png)?;
    tokio::fs::write(second_movie_dir.join("poster.png"), poster_png.get_ref()).await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let job_id = job.id.clone();
    let scan_jobs = jobs.clone();
    let mut scan = tokio::spawn(async move { scan_jobs.run_to_completion(&job_id, 1, None).await });

    let worker_claimed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status: Option<String> = sqlx::query_scalar(
                "SELECT status FROM scan_local_metadata_batches WHERE job_id = ? LIMIT 1",
            )
            .bind(&job.id)
            .fetch_optional(database.pool())
            .await?;
            if status.as_deref() == Some("RUNNING") {
                return Ok::<_, sqlx::Error>(true);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or(Ok(false))?;
    let mut fifo_writer = if worker_claimed {
        Some(
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&blocked_poster)
                    {
                        Ok(writer) => return Ok::<_, std::io::Error>(writer),
                        Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                        Err(error) => return Err(error),
                    }
                }
            })
            .await??,
        )
    } else {
        None
    };
    let worker_blocked = fifo_writer.is_some();
    let completed_scan = tokio::time::timeout(Duration::from_secs(30), &mut scan)
        .await
        .ok();
    let scan_finished_while_poster_blocked = completed_scan.is_some();

    // Always release the FIFO so a failed assertion cannot leave the worker stuck.
    if let Some(writer) = fifo_writer.as_mut() {
        writer.write_all(poster_png.get_ref())?;
        drop(fifo_writer.take());
    } else {
        tokio::fs::remove_file(&blocked_poster).await?;
    }
    if let Some(result) = completed_scan {
        result??;
    } else {
        scan.await??;
    }
    wait_for_local_metadata_batches(&database, &job.id).await?;

    let indexed: (i64, i64) = sqlx::query_as(
        "SELECT
             (SELECT COUNT(*) FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'),
             (SELECT COUNT(*) FROM item_images image
              JOIN media_items item ON item.id = image.item_id
              WHERE item.library_id = ? AND image.image_type = 'POSTER')",
    )
    .bind(library.id.to_string())
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert!(
        worker_blocked,
        "the local poster worker never reached the FIFO"
    );
    assert!(
        scan_finished_while_poster_blocked,
        "manifest scan must finish while the first local poster read is blocked"
    );
    assert_eq!(
        indexed,
        (2, 2),
        "both posters should appear after FIFO release"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_scan_during_running_local_image_claim_cancels_before_completeness()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{
        ffi::CString,
        io::Write,
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
        time::Duration,
    };

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Cancelled Image (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Cancelled.Image.2024.mkv"), b"movie").await?;
    let poster_path = movie_dir.join("poster.png");
    let poster_path_c = CString::new(poster_path.as_os_str().as_bytes())?;
    // SAFETY: the path is a valid NUL-terminated fixture path and mode is restrictive.
    let mkfifo_result = unsafe { libc::mkfifo(poster_path_c.as_ptr(), 0o600) };
    assert_eq!(mkfifo_result, 0, "failed to create blocked poster fixture");

    let mut poster_png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([76, 54, 32, 255]),
    ))
    .write_to(&mut poster_png, image::ImageFormat::Png)?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-UTF8 root")?)
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let mut queued = false;
    for _ in 0..20 {
        let report = jobs.run_batch(&job.id, 1).await?;
        let batch_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM scan_local_metadata_batches WHERE job_id = ?")
                .bind(&job.id)
                .fetch_one(database.pool())
                .await?;
        if batch_count > 0 {
            assert!(
                !report.completed,
                "fixture scan finished before cancellation"
            );
            queued = true;
            break;
        }
        assert!(
            !report.completed,
            "scan ended without publishing local image work"
        );
    }
    assert!(queued, "scan did not publish the local image batch");
    jobs.start_local_metadata_outbox_worker().await?;
    let claimed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let status: Option<String> = sqlx::query_scalar(
                "SELECT status FROM scan_local_metadata_batches WHERE job_id = ? LIMIT 1",
            )
            .bind(&job.id)
            .fetch_optional(database.pool())
            .await?;
            if status.as_deref() == Some("RUNNING") {
                return Ok::<_, sqlx::Error>(true);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or(Ok(false))?;
    let mut fifo_writer = if claimed {
        Some(
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&poster_path)
                    {
                        Ok(writer) => return Ok::<_, std::io::Error>(writer),
                        Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                        Err(error) => return Err(error),
                    }
                }
            })
            .await??,
        )
    } else {
        None
    };
    if claimed {
        jobs.cancel(&job.id).await?;
    }
    let cancelled = if claimed {
        jobs.run_batch(&job.id, 1).await?.status == "CANCELLED"
    } else {
        false
    };

    // Release the blocked image read even if cancellation did not reach the terminal state.
    if let Some(writer) = fifo_writer.as_mut() {
        writer.write_all(poster_png.get_ref())?;
        drop(fifo_writer.take());
    } else {
        tokio::fs::remove_file(&poster_path).await?;
    }
    let batch: (String, Option<i64>) = sqlx::query_as(
        "SELECT status, images_completed_at FROM scan_local_metadata_batches
         WHERE job_id = ? LIMIT 1",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let poster_completeness: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM item_metadata_completeness
         WHERE item_id = ? AND capability = 'POSTER'",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;

    assert!(claimed, "local metadata batch never entered RUNNING");
    assert!(cancelled, "scan job did not acknowledge cancellation");
    assert_eq!(
        batch.0, "CANCELLED",
        "claimed local work must be cancelled with its scan"
    );
    assert_eq!(
        batch.1, None,
        "cancelled image stage must not be marked complete"
    );
    assert_eq!(
        poster_completeness, 0,
        "cancellation must precede READY/missing commit"
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn unreadable_local_poster_keeps_image_check_retryable()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Unreadable Poster (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Unreadable.Poster.2024.mkv"), b"movie").await?;
    let poster_path = movie_dir.join("poster.png");
    let mut poster_png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([56, 34, 12, 255]),
    ))
    .write_to(&mut poster_png, image::ImageFormat::Png)?;
    tokio::fs::write(&poster_path, poster_png.get_ref()).await?;
    let mut permissions = tokio::fs::metadata(&poster_path).await?.permissions();
    permissions.set_mode(0o000);
    tokio::fs::set_permissions(&poster_path, permissions).await?;
    assert_eq!(
        tokio::fs::read(&poster_path)
            .await
            .expect_err("poster should be unreadable")
            .kind(),
        std::io::ErrorKind::PermissionDenied,
        "fixture must inject a real permission error"
    );

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let batch: (String, Option<i64>, Option<String>) =
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let row: Option<(String, Option<i64>, Option<String>)> = sqlx::query_as(
                    "SELECT status, images_completed_at, error
                     FROM scan_local_metadata_batches WHERE job_id = ? LIMIT 1",
                )
                .bind(&job.id)
                .fetch_optional(database.pool())
                .await?;
                if let Some(row) = row
                    && matches!(row.0.as_str(), "FAILED" | "COMPLETED")
                {
                    return Ok::<_, sqlx::Error>(row);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let completeness: Option<(String, Option<i64>)> = sqlx::query_as(
        "SELECT local_state, is_missing FROM item_metadata_completeness
         WHERE item_id = ? AND capability = 'POSTER'",
    )
    .bind(&item_id)
    .fetch_optional(database.pool())
    .await?;

    assert_eq!(
        batch.0, "FAILED",
        "permission failure must keep the image batch retryable"
    );
    assert_eq!(
        batch.1, None,
        "failed image reads must not complete image stage"
    );
    assert_eq!(
        completeness, None,
        "failed image reads must not confirm poster missing"
    );

    let mut permissions = tokio::fs::metadata(&poster_path).await?.permissions();
    permissions.set_mode(0o644);
    tokio::fs::set_permissions(&poster_path, permissions).await?;
    sqlx::query(
        "UPDATE scan_local_metadata_batches SET next_attempt_at = 0
         WHERE job_id = ? AND status = 'FAILED'",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await?;
    let retried = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let status: String = sqlx::query_scalar(
                "SELECT status FROM scan_local_metadata_batches WHERE job_id = ? LIMIT 1",
            )
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
            let poster: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND image_type = 'POSTER'",
            )
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
            if status == "COMPLETED" && poster == 1 {
                return Ok::<_, sqlx::Error>(true);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or(Ok(false))?;
    assert!(
        retried,
        "restoring poster permissions should let the batch retry"
    );
    Ok(())
}

#[tokio::test]
async fn unavailable_media_root_keeps_local_image_batch_retryable()
-> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Unavailable Root (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Unavailable.Root.2024.mkv"), b"movie").await?;
    let mut poster_png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([89, 67, 45, 255]),
    ))
    .write_to(&mut poster_png, image::ImageFormat::Png)?;
    tokio::fs::write(movie_dir.join("poster.png"), poster_png.get_ref()).await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-UTF8 root")?)
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let mut queued = false;
    for _ in 0..20 {
        let report = jobs.run_batch(&job.id, 100).await?;
        let batch_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM scan_local_metadata_batches WHERE job_id = ?")
                .bind(&job.id)
                .fetch_one(database.pool())
                .await?;
        if batch_count > 0 {
            queued = true;
            break;
        }
        assert!(
            !report.completed,
            "scan ended without publishing local image work"
        );
    }
    assert!(queued, "scan did not publish the local image batch");

    let offline_root = temp_dir.path().join("Movies.offline");
    tokio::fs::rename(&media_root, &offline_root).await?;
    jobs.start_local_metadata_outbox_worker().await?;
    let batch: (String, Option<i64>) = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let row: Option<(String, Option<i64>)> = sqlx::query_as(
                "SELECT status, images_completed_at FROM scan_local_metadata_batches
                 WHERE job_id = ? LIMIT 1",
            )
            .bind(&job.id)
            .fetch_optional(database.pool())
            .await?;
            if let Some(row) = row
                && matches!(row.0.as_str(), "FAILED" | "COMPLETED")
            {
                return Ok::<_, sqlx::Error>(row);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert_eq!(
        batch.0, "FAILED",
        "a missing root must leave the batch retryable"
    );
    assert_eq!(
        batch.1, None,
        "an unavailable root cannot complete image checks"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM item_metadata_completeness completeness
             JOIN media_items item ON item.id = completeness.item_id
             WHERE item.library_id = ? AND completeness.capability = 'POSTER'",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?,
        0,
        "an unavailable root cannot confirm poster availability or absence"
    );

    tokio::fs::rename(&offline_root, &media_root).await?;
    sqlx::query(
        "UPDATE scan_local_metadata_batches SET next_attempt_at = 0
         WHERE job_id = ? AND status = 'FAILED'",
    )
    .bind(&job.id)
    .execute(database.pool())
    .await?;
    let retried = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let status: String = sqlx::query_scalar(
                "SELECT status FROM scan_local_metadata_batches WHERE job_id = ? LIMIT 1",
            )
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
            let posters: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM item_images image
                 JOIN media_items item ON item.id = image.item_id
                 WHERE item.library_id = ? AND image.image_type = 'POSTER'",
            )
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
            if status == "COMPLETED" && posters == 1 {
                return Ok::<_, sqlx::Error>(true);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or(Ok(false))?;
    assert!(retried, "restoring the root should let the batch retry");
    jobs.run_to_completion(&job.id, 100, None).await?;
    Ok(())
}

#[tokio::test]
async fn completed_movie_scan_indexes_local_nfo_and_images()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(
        movie_dir.join("Example.Movie.2020.strm"),
        "https://example.invalid/media/example",
    )
    .await?;
    tokio::fs::write(
        movie_dir.join("Example.Movie.2020.nfo"),
        "<movie><title>Title From NFO</title><plot>Overview from NFO</plot><rating>8.4</rating></movie>",
    )
    .await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"poster").await?;
    tokio::fs::write(movie_dir.join("fanart.jpg"), b"fanart").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    wait_for_local_metadata_batches(&database, &job.id).await?;

    let item: (String, String, Option<f64>, Option<String>) = sqlx::query_as(
        "SELECT title, overview, rating, rating_source
         FROM media_items WHERE item_type = 'MOVIE'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        item,
        (
            "Title From NFO".to_owned(),
            "Overview from NFO".to_owned(),
            Some(8.4),
            Some("NFO".to_owned()),
        )
    );

    let images: Vec<(String, String)> =
        sqlx::query_as("SELECT image_type, local_path FROM item_images ORDER BY image_type")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].0, "FANART");
    assert_eq!(images[1].0, "POSTER");
    assert!(images.iter().all(|(_, path)| path.ends_with(".jpg")));
    Ok(())
}

#[tokio::test]
async fn local_nfo_identity_conflict_completes_batch_without_retry()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;

    tokio::fs::write(media_root.join("Target.Movie.1987.mkv"), b"movie-a").await?;
    tokio::fs::write(
        media_root.join("Target.Movie.1987.nfo"),
        "<movie><title>Target Movie</title><year>1987</year></movie>",
    )
    .await?;
    tokio::fs::write(media_root.join("Different.Movie.2018.mkv"), b"movie-b").await?;
    tokio::fs::write(
        media_root.join("Different.Movie.2018.nfo"),
        "<movie><title>Target Movie</title><year>1987</year></movie>",
    )
    .await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let batch = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let batch: (String, i64, Option<String>) = sqlx::query_as(
                "SELECT status, attempts, error
                 FROM scan_local_metadata_batches WHERE job_id = ?",
            )
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
            if !matches!(batch.0.as_str(), "PENDING" | "RUNNING") {
                return Ok::<_, sqlx::Error>(batch);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert_eq!(batch.0, "COMPLETED");
    assert_eq!(batch.1, 1);
    assert!(
        batch
            .2
            .as_deref()
            .is_some_and(|error| error.contains("non-retryable"))
    );
    Ok(())
}

#[tokio::test]
async fn local_nfo_retry_excludes_non_retryable_conflict_item()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    for (directory, file, nfo) in [
        (
            "00 Conflict Primary (1987)",
            "Conflict.Primary.1987.mkv",
            "<movie><title>Shared Conflict Movie</title><year>1987</year></movie>",
        ),
        (
            "01 Conflict Duplicate (2018)",
            "Conflict.Duplicate.2018.mkv",
            "<movie><title>Shared Conflict Movie</title><year>1987</year></movie>",
        ),
        (
            "02 Retry Target (2024)",
            "Retry.Target.2024.mkv",
            "<movie><title>Retry Target From NFO</title><year>2024</year></movie>",
        ),
    ] {
        let movie_dir = media_root.join(directory);
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(movie_dir.join(file), b"movie").await?;
        tokio::fs::write(movie_dir.join("movie.nfo"), nfo).await?;
    }

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?;
    sqlx::query(
        "CREATE TRIGGER fail_retry_target_nfo_update
         BEFORE UPDATE OF title ON media_items
         WHEN OLD.item_type = 'MOVIE' AND OLD.production_year = 2024
         BEGIN SELECT RAISE(ABORT, 'injected transient NFO failure'); END",
    )
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let batch: (String, i64, String, String) =
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let batch: (String, i64, String, String) = sqlx::query_as(
                    "SELECT status, attempts, source_refs_json, non_retryable_item_ids_json
                     FROM scan_local_metadata_batches WHERE job_id = ?",
                )
                .bind(&job.id)
                .fetch_one(database.pool())
                .await?;
                if batch.0 == "FAILED" {
                    return Ok::<_, sqlx::Error>(batch);
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await??;
    assert_eq!(batch.1, 1);

    let conflicting_item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items
         WHERE library_id = ? AND item_type = 'MOVIE'
           AND title <> 'Shared Conflict Movie' AND production_year IN (1987, 2018)",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let retry_item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items
         WHERE library_id = ? AND item_type = 'MOVIE' AND production_year = 2024",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let retry_entry_id: String = sqlx::query_scalar(
        "SELECT entry.id FROM filesystem_entries entry
         JOIN media_sources source ON source.filesystem_entry_id = entry.id
         WHERE source.item_id = ?",
    )
    .bind(&retry_item_id)
    .fetch_one(database.pool())
    .await?;
    let retry_source_ids: Vec<String> = serde_json::from_str(&batch.2)?;
    let excluded_item_ids: Vec<String> = serde_json::from_str(&batch.3)?;
    assert!(
        excluded_item_ids.contains(&conflicting_item_id),
        "a permanent NFO identity conflict must be persisted as excluded before retrying the batch"
    );
    assert!(
        retry_source_ids.contains(&retry_entry_id),
        "the transiently failing item must remain in the retry batch"
    );

    sqlx::query("DROP TRIGGER fail_retry_target_nfo_update")
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE scan_local_metadata_batches SET next_attempt_at = 0 WHERE job_id = ?")
        .bind(&job.id)
        .execute(database.pool())
        .await?;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status: String = sqlx::query_scalar(
                "SELECT status FROM scan_local_metadata_batches WHERE job_id = ?",
            )
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
            if status == "COMPLETED" {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn incremental_movie_scan_indexes_local_images() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;

    let movie_dir = media_root.join("Incremental Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Incremental.Movie.2024.mkv"), b"movie").await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"poster").await?;
    tokio::fs::write(movie_dir.join("fanart.jpg"), b"fanart").await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![luxd::application::scanner::IncrementalScanChange {
                root_id: root.id.to_string(),
                relative_path: "Incremental Movie (2024)".to_owned(),
                kind: luxd::application::watch::ChangeKind::Create,
            }],
        )
        .await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    wait_for_local_metadata_batches(&database, &job.id).await?;

    let images: Vec<(String, String)> =
        sqlx::query_as("SELECT image_type, local_path FROM item_images ORDER BY image_type")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].0, "FANART");
    assert_eq!(images[1].0, "POSTER");
    assert!(images.iter().all(|(_, path)| path.ends_with(".jpg")));
    Ok(())
}

#[tokio::test]
async fn incremental_sidecar_change_replaces_local_image() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Sidecar Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Sidecar.Movie.2024.mkv"), b"movie").await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"old-poster").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?
        .root;
    let jobs = ScanJobService::new(database.clone());
    let initial = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&initial.id, 100, None).await?;
    wait_for_local_metadata_batches(&database, &initial.id).await?;

    tokio::fs::remove_file(movie_dir.join("poster.jpg")).await?;
    tokio::fs::write(movie_dir.join("poster.webp"), b"new-poster").await?;
    let incremental = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root.id.to_string(),
                relative_path: "Sidecar Movie (2024)/poster.webp".to_owned(),
                kind: ChangeKind::Modify,
            }],
        )
        .await?;
    jobs.run_to_completion(&incremental.id, 100, None).await?;

    let poster_path: String =
        sqlx::query_scalar("SELECT local_path FROM item_images WHERE image_type = 'POSTER'")
            .fetch_one(database.pool())
            .await?;
    assert!(poster_path.ends_with("poster.webp"));
    Ok(())
}

#[tokio::test]
async fn workflow3_running_discovery_indexes_poster_added_to_unvisited_directory()
-> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let target_movie_dir = media_root.join("00 Late Poster Movie (2024)");
    let following_movie_dir = target_movie_dir.join("01 Following Movie (2024)");
    tokio::fs::create_dir_all(&target_movie_dir).await?;
    tokio::fs::create_dir_all(&following_movie_dir).await?;
    tokio::fs::write(
        target_movie_dir.join("Late.Poster.Movie.2024.mkv"),
        b"movie",
    )
    .await?;
    tokio::fs::write(
        following_movie_dir.join("Following.Movie.2024.mkv"),
        b"movie",
    )
    .await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?
        .root;
    let jobs = ScanJobService::new(database.clone());
    jobs.start_local_metadata_outbox_worker().await?;
    // Drain the startup backfill before discovery so it cannot race this job for the item.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let backfill_status: Option<String> = sqlx::query_scalar(
                "SELECT status FROM scan_local_metadata_backfills WHERE library_root_id = ?",
            )
            .bind(root.id.to_string())
            .fetch_optional(database.pool())
            .await?;
            if backfill_status.as_deref() == Some("COMPLETED") {
                return Ok::<(), sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    let job = jobs.create_movie_scan_job(library.id).await?;

    let root_batch = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(root_batch.status, "RUNNING");
    let scan_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    let manifest_state: String =
        sqlx::query_scalar("SELECT state FROM scan_manifests WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(scan_status, "RUNNING");
    assert_eq!(manifest_state, "DISCOVERING");

    let poster_path = target_movie_dir.join("poster.png");
    let mut poster_png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([12, 34, 56, 255]),
    ))
    .write_to(&mut poster_png, image::ImageFormat::Png)?;
    tokio::fs::write(&poster_path, poster_png.get_ref()).await?;

    let target_batch = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(target_batch.status, "RUNNING");
    let batch_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_local_metadata_batches WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert!(
        batch_count > 0,
        "the discovery batch publishes local image work"
    );
    wait_for_local_metadata_batches(&database, &job.id).await?;
    let target_media_relative_path = "00 Late Poster Movie (2024)/Late.Poster.Movie.2024.mkv";
    let item_id: String = sqlx::query_scalar(
        "SELECT source.item_id
         FROM media_sources source
         JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
         WHERE entry.relative_path = ?",
    )
    .bind(target_media_relative_path)
    .fetch_one(database.pool())
    .await?;
    let poster_paths: Vec<String> = sqlx::query_scalar(
        "SELECT local_path FROM item_images
         WHERE item_id = ? AND image_type = 'POSTER' ORDER BY image_index",
    )
    .bind(&item_id)
    .fetch_all(database.pool())
    .await?;
    let canonical_poster_paths: Vec<(String, Option<std::path::PathBuf>)> = poster_paths
        .iter()
        .map(|local_path| (local_path.clone(), std::fs::canonicalize(local_path).ok()))
        .collect();
    let expected_canonical_poster = std::fs::canonicalize(&poster_path)?;
    assert!(
        canonical_poster_paths
            .iter()
            .any(|(_, canonical_path)| canonical_path.as_ref() == Some(&expected_canonical_poster)),
        "the late poster is indexed for item {item_id} from {target_media_relative_path:?}: requested_path={poster_path:?}, canonical_requested_path={expected_canonical_poster:?}, poster_paths={canonical_poster_paths:?}"
    );
    let completed_image_batches: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_local_metadata_batches
         WHERE job_id = ? AND status = 'COMPLETED' AND images_completed_at IS NOT NULL",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert!(
        completed_image_batches > 0,
        "the same job's local image stage completed"
    );

    let scan_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    let manifest_state: String =
        sqlx::query_scalar("SELECT state FROM scan_manifests WHERE job_id = ?")
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(scan_status, "RUNNING");
    assert_eq!(manifest_state, "DISCOVERING");
    let following_item_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources source
         JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
         WHERE entry.relative_path = '00 Late Poster Movie (2024)/01 Following Movie (2024)/Following.Movie.2024.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        following_item_count, 0,
        "the following directory is not visited yet"
    );

    let following_batch = jobs.run_batch(&job.id, 1).await?;
    assert_eq!(following_batch.status, "RUNNING");
    let following_item_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources source
         JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
         WHERE entry.relative_path = '00 Late Poster Movie (2024)/01 Following Movie (2024)/Following.Movie.2024.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        following_item_count, 1,
        "the same job visits the following directory"
    );

    jobs.run_to_completion(&job.id, 1, None).await?;
    let final_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(final_status, "COMPLETED");
    Ok(())
}

#[tokio::test]
async fn movie_scan_indexes_multiple_emby_backdrops_in_order()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Multiple Backdrops (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Multiple.Backdrops.2024.mkv"), b"movie").await?;
    tokio::fs::write(movie_dir.join("backdrop.jpg"), b"backdrop-0").await?;
    tokio::fs::write(movie_dir.join("backdrop1.jpg"), b"backdrop-1").await?;
    tokio::fs::write(movie_dir.join("fanart-2.jpg"), b"backdrop-2").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    wait_for_local_metadata_batches(&database, &job.id).await?;

    let images: Vec<(i64, String)> = sqlx::query_as(
        "SELECT image_index, local_path
         FROM item_images
         WHERE image_type = 'FANART'
         ORDER BY image_index",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(images.len(), 3);
    assert_eq!(images[0].0, 0);
    assert!(images[0].1.ends_with("backdrop.jpg"));
    assert_eq!(images[1].0, 1);
    assert!(images[1].1.ends_with("backdrop1.jpg"));
    assert_eq!(images[2].0, 2);
    assert!(images[2].1.ends_with("fanart-2.jpg"));
    Ok(())
}

#[tokio::test]
async fn completed_flat_movie_scan_indexes_media_prefixed_images_per_item()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    for (stem, poster, backdrop) in [
        ("Flat.One.2020", "poster.png", "fanart.jpg"),
        ("Flat.Two.2021", "poster.png", "backdrop.jpg"),
    ] {
        tokio::fs::write(
            media_root.join(format!("{stem}.strm")),
            "https://example.invalid/media/flat",
        )
        .await?;
        tokio::fs::write(media_root.join(format!("{stem}-{poster}")), b"poster").await?;
        tokio::fs::write(media_root.join(format!("{stem}-{backdrop}")), b"backdrop").await?;
    }

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    wait_for_local_metadata_batches(&database, &job.id).await?;

    let images: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT media_items.sort_title, item_images.image_type, item_images.local_path
         FROM item_images
         JOIN media_items ON media_items.id = item_images.item_id
         ORDER BY media_items.sort_title, item_images.image_type",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(images.len(), 4);
    assert!(images.iter().any(|(title, image_type, path)| {
        title == "flat one" && image_type == "POSTER" && path.ends_with("Flat.One.2020-poster.png")
    }));
    assert!(images.iter().any(|(title, image_type, path)| {
        title == "flat one" && image_type == "FANART" && path.ends_with("Flat.One.2020-fanart.jpg")
    }));
    assert!(images.iter().any(|(title, image_type, path)| {
        title == "flat two" && image_type == "POSTER" && path.ends_with("Flat.Two.2021-poster.png")
    }));
    assert!(images.iter().any(|(title, image_type, path)| {
        title == "flat two"
            && image_type == "FANART"
            && path.ends_with("Flat.Two.2021-backdrop.jpg")
    }));
    Ok(())
}

#[tokio::test]
async fn rescan_updates_an_existing_local_image_path() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Updated Movie (2026)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Updated.Movie.2026.mkv"), b"movie").await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"old-poster").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let first_job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first_job.id, 100, None).await?;
    wait_for_local_metadata_batches(&database, &first_job.id).await?;
    let first_image: (String, Option<String>) = sqlx::query_as(
        "SELECT local_path, content_tag FROM item_images WHERE image_type = 'POSTER'",
    )
    .fetch_one(database.pool())
    .await?;

    tokio::fs::remove_file(movie_dir.join("poster.jpg")).await?;
    tokio::fs::write(movie_dir.join("poster.webp"), b"new-poster").await?;
    let second_job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&second_job.id, 100, None).await?;
    wait_for_local_metadata_batches(&database, &second_job.id).await?;

    let second_image: (String, Option<String>) = sqlx::query_as(
        "SELECT local_path, content_tag FROM item_images WHERE image_type = 'POSTER'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(second_image.0.ends_with("poster.webp"));
    assert!(first_image.1.is_some());
    assert!(second_image.1.is_some());
    assert_ne!(first_image.1, second_image.1);
    Ok(())
}

#[tokio::test]
async fn completed_mixed_scan_indexes_local_movie_and_series_images()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Mixed");
    let movie_dir = media_root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(
        movie_dir.join("Example.Movie.2020.strm"),
        "https://example.invalid/media/movie",
    )
    .await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"movie-poster").await?;
    tokio::fs::write(movie_dir.join("fanart.jpg"), b"movie-fanart").await?;

    let series_dir = media_root.join("Example Show");
    let season_dir = series_dir.join("Season 01");
    tokio::fs::create_dir_all(&season_dir).await?;
    tokio::fs::write(
        series_dir.join("tvshow.nfo"),
        "<tvshow><title>Example Show</title></tvshow>",
    )
    .await?;
    tokio::fs::write(
        season_dir.join("Example.Show.S01E01.strm"),
        "https://example.invalid/media/episode",
    )
    .await?;
    tokio::fs::write(series_dir.join("poster.jpg"), b"series-poster").await?;
    tokio::fs::write(series_dir.join("fanart.jpg"), b"series-fanart").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Mixed", LibraryKind::Mixed, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    wait_for_local_metadata_batches(&database, &job.id).await?;

    let images: Vec<(String, String)> = sqlx::query_as(
        "SELECT media_items.item_type, item_images.image_type
         FROM item_images
         JOIN media_items ON media_items.id = item_images.item_id
         ORDER BY media_items.item_type, item_images.image_type",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        images,
        vec![
            ("MOVIE".to_owned(), "FANART".to_owned()),
            ("MOVIE".to_owned(), "POSTER".to_owned()),
            ("SERIES".to_owned(), "FANART".to_owned()),
            ("SERIES".to_owned(), "POSTER".to_owned()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn failed_local_poster_insert_does_not_mark_image_stage_complete()
-> Result<(), Box<dyn std::error::Error>> {
    use std::io::Cursor;

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let mut poster_png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([255, 0, 0, 255]),
    ))
    .write_to(&mut poster_png, image::ImageFormat::Png)?;
    tokio::fs::create_dir_all(&media_root).await?;
    for media_name in [
        "First.Poster.Insert.2026",
        "Second.Poster.Insert.2026",
        "Third.Poster.Insert.2026",
    ] {
        tokio::fs::write(media_root.join(format!("{media_name}.mkv")), b"movie").await?;
        tokio::fs::write(
            media_root.join(format!("{media_name}-poster.png")),
            poster_png.get_ref(),
        )
        .await?;
    }

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    sqlx::query(
        "CREATE TRIGGER fail_local_poster_insert
         BEFORE INSERT ON item_images
         WHEN NEW.image_type = 'POSTER'
           AND (SELECT COUNT(*) FROM item_images
                WHERE source = 'LOCAL' AND image_type = 'POSTER') >= 2
         BEGIN SELECT RAISE(ABORT, 'injected poster insert failure'); END",
    )
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone());
    jobs.start_local_metadata_outbox_worker().await?;
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let batch_id = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let batch: Option<(String, Option<i64>, i64)> = sqlx::query_as(
                "SELECT id, images_completed_at, source_count FROM scan_local_metadata_batches
                 WHERE job_id = ? ORDER BY created_at, id LIMIT 1",
            )
            .bind(&job.id)
            .fetch_optional(database.pool())
            .await?;
            if let Some((id, images_completed_at, source_count)) = batch {
                let status: String = sqlx::query_scalar(
                    "SELECT status FROM scan_local_metadata_batches WHERE id = ?",
                )
                .bind(&id)
                .fetch_one(database.pool())
                .await?;
                if status == "FAILED" {
                    assert!(
                        source_count >= 3,
                        "all movie image sources should share the outbox page"
                    );
                    assert_eq!(images_completed_at, None);
                    return Ok::<_, sqlx::Error>(id);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    let image_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM item_images WHERE source = 'LOCAL'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        image_count, 1,
        "failure must roll back the second image page but preserve the early first poster"
    );

    sqlx::query("DROP TRIGGER fail_local_poster_insert")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "UPDATE scan_local_metadata_batches
         SET next_attempt_at = 0, updated_at = unixepoch() WHERE id = ? AND status = 'FAILED'",
    )
    .bind(&batch_id)
    .execute(database.pool())
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let status: String =
                sqlx::query_scalar("SELECT status FROM scan_local_metadata_batches WHERE id = ?")
                    .bind(&batch_id)
                    .fetch_one(database.pool())
                    .await?;
            if status == "COMPLETED" {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    let image_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM item_images WHERE source = 'LOCAL'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(image_count, 3, "retry should index all local posters");
    Ok(())
}

#[tokio::test]
async fn local_metadata_refresh_reindexes_only_the_requested_directories()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    for (directory, file) in [
        ("Alpha (2020)", "Alpha.2020.mkv"),
        ("Beta (2021)", "Beta.2021.mkv"),
    ] {
        let movie_dir = media_root.join(directory);
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(movie_dir.join(file), b"movie").await?;
        tokio::fs::write(movie_dir.join("poster.jpg"), b"jpg").await?;
    }

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF8 media root")?,
        )
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let open: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM scan_local_metadata_batches
                 WHERE status IN ('PENDING', 'RUNNING')",
            )
            .fetch_one(database.pool())
            .await?;
            if open == 0 {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    let posters = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(DISTINCT item_id) FROM item_images WHERE image_type = 'POSTER'",
        )
        .fetch_one(database.pool())
        .await
    };
    assert_eq!(posters().await?, 2, "the initial scan indexes both posters");

    // Simulate items that lost their images although the files on disk are unchanged.
    sqlx::query("DELETE FROM item_images")
        .execute(database.pool())
        .await?;
    assert_eq!(posters().await?, 0);

    let accepted = jobs
        .start_local_metadata_refresh(library.id, None, &["Alpha (2020)".to_owned()])
        .await?;
    assert_eq!(accepted.directories, 1);
    assert_eq!(accepted.entries, 1);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if posters().await? == 1 {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;

    let restored_title: String = sqlx::query_scalar(
        "SELECT mi.title FROM media_items mi
         JOIN item_images i ON i.item_id = mi.id AND i.image_type = 'POSTER'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(restored_title.starts_with("Alpha"), "{restored_title}");
    // The other directory was not part of the request and stays untouched.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(posters().await?, 1);

    // Invalid paths are rejected instead of silently matching nothing.
    assert!(
        jobs.start_local_metadata_refresh(library.id, None, &["../escape".to_owned()])
            .await
            .is_err()
    );
    assert!(
        jobs.start_local_metadata_refresh(library.id, None, &[])
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn nfo_retry_skips_the_completed_scan_image_stage() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(media_root.join("Retry.Nfo.Movie.2026.mkv"), b"movie").await?;
    tokio::fs::write(
        media_root.join("Retry.Nfo.Movie.2026.nfo"),
        "<movie><title>Recovered From NFO</title></movie>",
    )
    .await?;
    tokio::fs::write(
        media_root.join("Retry.Nfo.Movie.2026-poster.jpg"),
        b"poster",
    )
    .await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    sqlx::query(
        "CREATE TRIGGER fail_scan_nfo_update
         BEFORE UPDATE OF title ON media_items
         WHEN OLD.item_type = 'MOVIE'
         BEGIN SELECT RAISE(ABORT, 'injected scan NFO failure'); END",
    )
    .execute(database.pool())
    .await?;

    let jobs = ScanJobService::new(database.clone());
    jobs.start_local_metadata_outbox_worker().await?;
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let batch_id = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let batch: Option<(String, String, Option<i64>)> = sqlx::query_as(
                "SELECT id, status, images_completed_at FROM scan_local_metadata_batches
                 WHERE job_id = ? ORDER BY created_at, id LIMIT 1",
            )
            .bind(&job.id)
            .fetch_optional(database.pool())
            .await?;
            if let Some((id, status, images_completed_at)) = batch
                && status == "FAILED"
            {
                assert!(images_completed_at.is_some());
                return Ok::<_, sqlx::Error>(id);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM item_images WHERE source = 'LOCAL'")
            .fetch_one(database.pool())
            .await?,
        1
    );

    sqlx::query("DROP TRIGGER fail_scan_nfo_update")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "CREATE TRIGGER reject_scan_image_retry
         BEFORE INSERT ON item_images
         BEGIN SELECT RAISE(ABORT, 'completed image stage must not run again'); END",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE scan_local_metadata_batches SET next_attempt_at = 0
         WHERE id = ? AND status = 'FAILED'",
    )
    .bind(&batch_id)
    .execute(database.pool())
    .await?;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let status: String =
                sqlx::query_scalar("SELECT status FROM scan_local_metadata_batches WHERE id = ?")
                    .bind(&batch_id)
                    .fetch_one(database.pool())
                    .await?;
            if status == "COMPLETED" {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await??;
    Ok(())
}
