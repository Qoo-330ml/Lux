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
