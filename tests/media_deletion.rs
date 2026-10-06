use std::{fs, path::Path, sync::Arc};

use luxd::{
    application::{
        deletion::MediaDeleteService, libraries::LibraryService, scanner::LibraryScanner,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use tokio::sync::Barrier;

#[tokio::test]
async fn concurrent_media_deletions_leave_no_orphaned_file()
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
    fs::create_dir_all(&root)?;
    let media_file = root.join("Example.Movie.2024.mkv");
    fs::write(&media_file, b"fixture")?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-UTF-8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let (item_id, source_id): (String, String) = sqlx::query_as(
        "SELECT mi.id, ms.id FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         WHERE mi.library_id = ? AND mi.item_type = 'MOVIE' LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;

    let deletion = MediaDeleteService::new(database.clone());
    let start = Arc::new(Barrier::new(3));
    let [first_request, second_request] = [(), ()].map(|_| {
        let deletion = deletion.clone();
        let start = Arc::clone(&start);
        let item_id = item_id.clone();
        let source_id = source_id.clone();
        tokio::spawn(async move {
            start.wait().await;
            deletion.delete(&item_id, Some(&source_id)).await
        })
    });
    start.wait().await;
    let first = first_request.await?;
    let second = second_request.await?;

    assert_ne!(first.is_ok(), second.is_ok());
    let remaining_sources: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_sources WHERE id = ?")
            .bind(source_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_sources, 0);
    assert!(!media_file.exists());
    assert_no_staged_deletion_files(&root)?;
    Ok(())
}

fn assert_no_staged_deletion_files(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    for entry in fs::read_dir(root)? {
        let name = entry?.file_name();
        assert!(!name.to_string_lossy().starts_with(".lux-delete-"));
    }
    Ok(())
}
