use luxd::{
    application::{libraries::LibraryService, scanner::LibraryScanner},
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[tokio::test]
async fn mixed_scan_classifies_movies_series_and_unresolved_without_cross_contamination()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Mixed");
    let movie_dir = root.join("Known Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Known.Movie.2020.mkv"), b"movie").await?;
    let series_dir = root.join("Known Show/Season 01");
    tokio::fs::create_dir_all(&series_dir).await?;
    tokio::fs::write(
        series_dir.parent().unwrap().join("tvshow.nfo"),
        "<tvshow><title>Known Show</title></tvshow>",
    )
    .await?;
    tokio::fs::write(series_dir.join("Known.Show.S01E01.mkv"), b"episode").await?;
    let unresolved_dir = root.join("Unclear");
    tokio::fs::create_dir_all(&unresolved_dir).await?;
    tokio::fs::write(unresolved_dir.join("Mystery File.mkv"), b"unknown").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Mixed", LibraryKind::Mixed, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    let first = scanner.scan_mixed_library(library.id).await?;
    assert_eq!(first.created_items, 5);
    assert_eq!(first.created_sources, 3);

    let counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT item_type, COUNT(*) FROM media_items GROUP BY item_type ORDER BY item_type",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        counts,
        vec![
            ("EPISODE".to_owned(), 1),
            ("MOVIE".to_owned(), 1),
            ("SEASON".to_owned(), 1),
            ("SERIES".to_owned(), 1),
            ("UNRESOLVED".to_owned(), 1),
        ]
    );
    let unresolved_status: String = sqlx::query_scalar(
        "SELECT identification_status FROM media_items WHERE item_type = 'UNRESOLVED'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(unresolved_status, "PENDING");
    let hierarchy: (String, String, String) = sqlx::query_as(
        "SELECT episode.parent_id, episode.series_id, season.parent_id
         FROM media_items episode
         JOIN media_items season ON season.id = episode.parent_id
         WHERE episode.item_type = 'EPISODE'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_ne!(hierarchy.0, hierarchy.2);
    assert_eq!(hierarchy.1, hierarchy.2);

    let ids_before: Vec<(String, String)> =
        sqlx::query_as("SELECT identity_key, id FROM media_items ORDER BY identity_key")
            .fetch_all(database.pool())
            .await?;
    let second = scanner.scan_mixed_library(library.id).await?;
    assert_eq!(second.created_items, 0);
    assert_eq!(second.created_sources, 0);
    assert_eq!(second.skipped_files, 3);
    let ids_after: Vec<(String, String)> =
        sqlx::query_as("SELECT identity_key, id FROM media_items ORDER BY identity_key")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(ids_before, ids_after);
    Ok(())
}
