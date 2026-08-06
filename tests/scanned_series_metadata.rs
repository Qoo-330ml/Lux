use luxd::{
    application::{libraries::LibraryService, scanner::ScanJobService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};

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
