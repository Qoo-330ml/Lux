use luxd::{
    application::libraries::LibraryService, application::scanner::ScanJobService, config::Config,
    library::LibraryKind, storage::Database,
};

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
        "<movie><title>Title From NFO</title><plot>Overview from NFO</plot></movie>",
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

    let item: (String, String) =
        sqlx::query_as("SELECT title, overview FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        item,
        ("Title From NFO".to_owned(), "Overview from NFO".to_owned())
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
