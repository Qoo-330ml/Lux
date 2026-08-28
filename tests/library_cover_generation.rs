use std::{fs, io::Cursor, path::Path, time::Duration};

use image::{DynamicImage, GenericImageView, ImageFormat, Rgba, RgbaImage};

use luxd::{
    application::{
        libraries::LibraryService,
        library_covers::{
            AUTO_LIBRARY_COVER_POSTER_COUNT, AutoLibraryCoverResult, LibraryCoverService,
        },
        scanner::ScanJobService,
        thumbnails::ThumbnailService,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use uuid::Uuid;

fn png_1x1() -> Result<Vec<u8>, image::ImageError> {
    let image = RgbaImage::from_pixel(1, 1, Rgba([32, 96, 160, 255]));
    let mut bytes = Vec::new();
    DynamicImage::ImageRgba8(image).write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)?;
    Ok(bytes)
}

fn config(root: &Path) -> Config {
    Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address is valid"),
        config_dir: root.join("config"),
    }
}

async fn add_posters(
    database: &Database,
    library_id: &str,
    directory: &Path,
    count: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(directory)?;
    let png = png_1x1()?;
    for index in 0..count {
        let item_id = Uuid::now_v7().to_string();
        let poster_path = directory.join(format!("poster-{index}.png"));
        fs::write(&poster_path, &png)?;
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'PENDING')",
        )
        .bind(&item_id)
        .bind(library_id)
        .bind(format!("Movie {index}"))
        .bind(format!("movie {index}"))
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO item_images (
                id, item_id, image_type, image_index, local_path, file_size, source
             ) VALUES (?, ?, 'POSTER', 0, ?, ?, 'LOCAL')",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(&item_id)
        .bind(poster_path.to_string_lossy().as_ref())
        .bind(i64::try_from(png.len())?)
        .execute(database.pool())
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn auto_cover_waits_for_nine_posters_then_runs_only_once()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("中文媒体库", LibraryKind::Movie, true)
        .await?;
    let scan_root = temp_dir.path().join("Movies");
    fs::create_dir_all(&scan_root)?;
    libraries
        .add_root(library.id, scan_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    let poster_dir = scan_root.join("posters");
    add_posters(&database, &library.id.to_string(), &poster_dir, 8).await?;

    let covers = LibraryCoverService::new(
        database.clone(),
        temp_dir.path().join("config/library-covers"),
    );
    assert_eq!(
        covers.generate_if_eligible(library.id).await?,
        AutoLibraryCoverResult::BelowThreshold
    );
    let registered: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scheduled_task_configs
         WHERE owner_type = 'LIBRARY' AND owner_id = ? AND task_type = 'AUTO_LIBRARY_COVER'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(registered, 0);

    add_posters(&database, &library.id.to_string(), &poster_dir, 1).await?;
    let jobs = ScanJobService::new(database.clone()).with_library_covers(covers.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;

    let generated_path = LibraryService::new(database.clone())
        .list_libraries()
        .await?
        .into_iter()
        .find(|view| view.library.id == library.id)
        .and_then(|view| view.library.cover_image_path)
        .expect("generated cover path");
    assert!(generated_path.contains("auto"));
    assert!(
        temp_dir
            .path()
            .join("config/library-covers")
            .join(&generated_path)
            .is_file()
    );
    let generated = fs::read(
        temp_dir
            .path()
            .join("config/library-covers")
            .join(&generated_path),
    )?;
    let generated_image = image::load_from_memory(&generated)?;
    assert_eq!(generated_image.dimensions(), (1280, 720));

    assert_eq!(
        covers.run_manually(library.id).await?,
        AutoLibraryCoverResult::Generated
    );
    let cover_jobs: Vec<(i64, String, i64, i64)> = sqlx::query_as(
        "SELECT is_manual, status, processed_count, total_count
         FROM library_cover_jobs WHERE library_id = ? ORDER BY created_at, id",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        cover_jobs,
        vec![
            (0, "COMPLETED".to_owned(), 1, 1),
            (1, "COMPLETED".to_owned(), 1, 1),
        ]
    );

    let manual_png = png_1x1()?;
    let manual = covers.store(library.id, "image/png", &manual_png).await?;
    assert_eq!(fs::read(manual.path)?, manual_png);
    assert!(
        !temp_dir
            .path()
            .join("config/library-covers")
            .join(generated_path)
            .exists()
    );

    assert_eq!(
        covers.generate_if_eligible(library.id).await?,
        AutoLibraryCoverResult::ExistingCover
    );
    let registered: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scheduled_task_configs
         WHERE owner_type = 'LIBRARY' AND owner_id = ? AND task_type = 'AUTO_LIBRARY_COVER'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(registered, 1);
    let task: (Option<String>, i64, String) = sqlx::query_as(
        "SELECT cron_or_interval, is_enabled, resource_limit_json
         FROM scheduled_task_configs
         WHERE owner_type = 'LIBRARY' AND owner_id = ? AND task_type = 'AUTO_LIBRARY_COVER'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(task, (None, 0, r#"{}"#.to_owned()));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn auto_cover_runs_before_unrelated_postprocessing_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config(temp_dir.path());
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("中文电视剧", LibraryKind::Series, true)
        .await?;
    let scan_root = temp_dir.path().join("Shows");
    fs::create_dir_all(&scan_root)?;
    LibraryService::new(database.clone())
        .add_root(library.id, scan_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    add_posters(
        &database,
        &library.id.to_string(),
        &scan_root.join("posters"),
        AUTO_LIBRARY_COVER_POSTER_COUNT,
    )
    .await?;
    fs::write(scan_root.join("Broken.Show.S01E01.mkv"), b"fixture")?;

    let covers = LibraryCoverService::new(
        database.clone(),
        temp_dir.path().join("config/library-covers"),
    );
    let thumbnails =
        ThumbnailService::with_runner(database.clone(), "false", Duration::from_secs(5));
    let jobs = ScanJobService::new(database.clone()).with_library_covers(covers);
    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion_with_metadata_and_thumbnails(&job.id, 100, None, None, Some(thumbnails))
        .await?;

    let scan_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(scan_status, "FAILED");

    let cover_path: Option<String> =
        sqlx::query_scalar("SELECT cover_image_path FROM libraries WHERE id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert!(
        cover_path.is_some(),
        "cover generation must not depend on thumbnails"
    );

    let registered: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scheduled_task_configs
         WHERE owner_type = 'LIBRARY' AND owner_id = ? AND task_type = 'AUTO_LIBRARY_COVER'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(registered, 1);
    Ok(())
}

#[tokio::test]
async fn auto_cover_reconciles_existing_eligible_libraries()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config(temp_dir.path());
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("已有海报的电视剧", LibraryKind::Series, true)
        .await?;
    let scan_root = temp_dir.path().join("Shows");
    fs::create_dir_all(&scan_root)?;
    libraries
        .add_root(library.id, scan_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    add_posters(
        &database,
        &library.id.to_string(),
        &scan_root.join("posters"),
        AUTO_LIBRARY_COVER_POSTER_COUNT,
    )
    .await?;

    let covers = LibraryCoverService::new(
        database.clone(),
        temp_dir.path().join("config/library-covers"),
    );
    assert_eq!(covers.reconcile_auto_library_covers().await?, 1);

    let cover_path: Option<String> =
        sqlx::query_scalar("SELECT cover_image_path FROM libraries WHERE id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert!(cover_path.is_some());
    Ok(())
}

#[tokio::test]
async fn auto_cover_reads_posters_from_metadata_library() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = config(temp_dir.path());
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("中文媒体库", LibraryKind::Movie, true)
        .await?;
    let scan_root = temp_dir.path().join("Movies");
    fs::create_dir_all(&scan_root)?;
    libraries
        .add_root(library.id, scan_root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let metadata_library = config.config_dir.join("metadata/library/aa");
    add_posters(
        &database,
        &library.id.to_string(),
        &metadata_library,
        AUTO_LIBRARY_COVER_POSTER_COUNT,
    )
    .await?;
    let covers =
        LibraryCoverService::new(database.clone(), config.config_dir.join("library-covers"))
            .with_metadata_directory(config.config_dir.join("metadata"));

    assert_eq!(
        covers.generate_if_eligible(library.id).await?,
        AutoLibraryCoverResult::Generated
    );
    let generated_path = LibraryService::new(database)
        .list_libraries()
        .await?
        .into_iter()
        .find(|view| view.library.id == library.id)
        .and_then(|view| view.library.cover_image_path)
        .ok_or("generated cover path")?;
    assert!(
        config
            .config_dir
            .join("library-covers")
            .join(generated_path)
            .is_file()
    );
    Ok(())
}

#[tokio::test]
async fn uploaded_cover_prevents_auto_registration_and_replacement()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&config(temp_dir.path())).await?;
    let library = LibraryService::new(database.clone())
        .create_library("中文媒体库", LibraryKind::Movie, true)
        .await?;
    let poster_dir = temp_dir.path().join("posters");
    add_posters(&database, &library.id.to_string(), &poster_dir, 9).await?;

    let covers = LibraryCoverService::new(
        database.clone(),
        temp_dir.path().join("config/library-covers"),
    );
    let png = png_1x1()?;
    let manual = covers.store(library.id, "image/png", &png).await?;
    assert_eq!(
        covers.generate_if_eligible(library.id).await?,
        AutoLibraryCoverResult::ExistingCover
    );
    assert_eq!(fs::read(manual.path)?, png);

    let registered: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scheduled_task_configs
         WHERE owner_type = 'LIBRARY' AND owner_id = ? AND task_type = 'AUTO_LIBRARY_COVER'",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(registered, 0);
    Ok(())
}
