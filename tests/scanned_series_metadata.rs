use luxd::{
    application::{libraries::LibraryService, scanner::ScanJobService},
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
async fn completed_series_scan_indexes_local_nfo_and_images()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    let series_dir = root.join("Example Show (2024)");
    let season_dir = series_dir.join("Season 01");
    tokio::fs::create_dir_all(&season_dir).await?;
    tokio::fs::write(
        series_dir.join("tvshow.nfo"),
        "<tvshow><title>Title From NFO</title><plot>Series overview</plot></tvshow>",
    )
    .await?;
    tokio::fs::write(series_dir.join("poster.jpg"), b"series-poster").await?;
    tokio::fs::write(series_dir.join("fanart.jpg"), b"series-fanart").await?;
    tokio::fs::write(
        season_dir.join("season.nfo"),
        "<season><title>Season From NFO</title></season>",
    )
    .await?;
    tokio::fs::write(season_dir.join("poster.jpg"), b"season-poster").await?;
    tokio::fs::write(season_dir.join("fanart.jpg"), b"season-fanart").await?;
    tokio::fs::write(
        season_dir.join("Example.Show.S01E01-thumb.jpg"),
        b"episode-thumb",
    )
    .await?;
    tokio::fs::write(
        season_dir.join("Example.Show.S01E01.strm"),
        "https://example.invalid/series/episode",
    )
    .await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    wait_for_local_metadata_batches(&database, &job.id).await?;

    let series_title: String =
        sqlx::query_scalar("SELECT title FROM media_items WHERE item_type = 'SERIES'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(series_title, "Title From NFO");

    let image_rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT item_type, image_type, local_path
         FROM item_images JOIN media_items ON media_items.id = item_images.item_id
         ORDER BY item_type, image_type",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(image_rows.len(), 5);
    assert_eq!(
        image_rows
            .iter()
            .map(|row| (row.0.as_str(), row.1.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("EPISODE", "THUMB"),
            ("SEASON", "FANART"),
            ("SEASON", "POSTER"),
            ("SERIES", "FANART"),
            ("SERIES", "POSTER")
        ]
    );
    assert!(image_rows.iter().all(|(_, _, path)| path.ends_with(".jpg")));

    let first_poster: (String, Option<String>) = sqlx::query_as(
        "SELECT item_images.local_path, item_images.content_tag
         FROM item_images JOIN media_items ON media_items.id = item_images.item_id
         WHERE media_items.item_type = 'SERIES' AND item_images.image_type = 'POSTER'",
    )
    .fetch_one(database.pool())
    .await?;
    tokio::fs::remove_file(series_dir.join("poster.jpg")).await?;
    tokio::fs::write(series_dir.join("poster.png"), b"updated-series-poster").await?;

    let rescan = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&rescan.id, 100, None).await?;
    wait_for_local_metadata_batches(&database, &rescan.id).await?;

    let updated_poster: (String, Option<String>) = sqlx::query_as(
        "SELECT item_images.local_path, item_images.content_tag
         FROM item_images JOIN media_items ON media_items.id = item_images.item_id
         WHERE media_items.item_type = 'SERIES' AND item_images.image_type = 'POSTER'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(updated_poster.0.ends_with("poster.png"));
    assert_ne!(first_poster.1, updated_poster.1);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn unreadable_series_poster_keeps_local_image_batch_retryable()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    let series_dir = root.join("Unreadable Show (2024)");
    let season_dir = series_dir.join("Season 01");
    tokio::fs::create_dir_all(&season_dir).await?;
    tokio::fs::write(
        series_dir.join("tvshow.nfo"),
        "<tvshow><title>Unreadable Show</title></tvshow>",
    )
    .await?;
    let poster_path = series_dir.join("poster.png");
    let mut poster_png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([23, 45, 67, 255]),
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
    tokio::fs::write(season_dir.join("Unreadable.Show.S01E01.mkv"), b"episode").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-UTF8 root")?)
        .await?;
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

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
    let series_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'SERIES'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        batch.0, "FAILED",
        "image read error must fail the local batch"
    );
    assert_eq!(batch.1, None, "image stage must remain retryable");
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM item_metadata_completeness
             WHERE item_id = ? AND capability = 'POSTER'",
        )
        .bind(&series_id)
        .fetch_one(database.pool())
        .await?,
        0,
        "permission failure must not confirm the poster capability"
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
            let poster_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM item_images WHERE item_id = ? AND image_type = 'POSTER'",
            )
            .bind(&series_id)
            .fetch_one(database.pool())
            .await?;
            if status == "COMPLETED" && poster_count == 1 {
                return Ok::<_, sqlx::Error>(true);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or(Ok(false))?;
    assert!(
        retried,
        "restoring permissions should let the series poster retry"
    );
    Ok(())
}

#[tokio::test]
async fn series_scan_indexes_images_in_nested_categories_after_one_nfo_conflict()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    for series_name in ["First Show (2024)", "Second Show (2024)"] {
        let series_dir = root.join("Drama").join(series_name);
        let season_dir = series_dir.join("Season 01");
        tokio::fs::create_dir_all(&season_dir).await?;
        tokio::fs::write(
            series_dir.join("tvshow.nfo"),
            "<tvshow><title>Shared Title</title><year>2024</year></tvshow>",
        )
        .await?;
        tokio::fs::write(series_dir.join("poster.jpg"), b"series-poster").await?;
        tokio::fs::write(series_dir.join("fanart.jpg"), b"series-fanart").await?;
        tokio::fs::write(season_dir.join("poster.jpg"), b"season-poster").await?;
        tokio::fs::write(season_dir.join("fanart.jpg"), b"season-fanart").await?;
        tokio::fs::write(
            season_dir.join(format!("{series_name}.S01E01.strm")),
            "https://example.invalid/series/episode",
        )
        .await?;
    }

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    wait_for_local_metadata_batches(&database, &job.id).await?;

    let image_rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT media_items.item_type, item_images.image_type
         FROM item_images JOIN media_items ON media_items.id = item_images.item_id
         ORDER BY media_items.item_type, item_images.image_type, item_images.item_id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(image_rows.len(), 8);
    assert_eq!(image_rows.iter().filter(|row| row.0 == "SERIES").count(), 4);
    assert_eq!(image_rows.iter().filter(|row| row.0 == "SEASON").count(), 4);
    Ok(())
}
