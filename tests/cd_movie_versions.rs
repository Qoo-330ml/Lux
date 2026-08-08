use luxd::{
    application::libraries::LibraryService, application::scanner::LibraryScanner, config::Config,
    library::LibraryKind, storage::Database,
};

#[tokio::test]
async fn movie_scan_groups_cd_parts_into_one_item_with_multiple_sources()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    for part in 1..=3 {
        tokio::fs::write(
            root.join(format!("FC22378556无码 cd{part}.mp4")),
            format!("cd{part}"),
        )
        .await?;
    }

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let report = LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    assert_eq!(report.created_items, 1);
    assert_eq!(report.created_sources, 3);
    let item_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items WHERE title = 'FC22378556无码' AND removed_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;
    let source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources WHERE item_id = (
             SELECT id FROM media_items WHERE title = 'FC22378556无码' AND removed_at IS NULL
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(item_count, 1);
    assert_eq!(source_count, 3);
    Ok(())
}
