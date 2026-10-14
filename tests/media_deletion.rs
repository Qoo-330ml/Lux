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
    let danmaku_file = root.join("Example.Movie.2024.xml");
    let mediainfo_file = root.join("Example.Movie.2024-mediainfo.json");
    let other_danmaku_file = root.join("Example.Movie.2024.Extended.xml");
    let other_mediainfo_file = root.join("Example.Movie.2024.Extended-mediainfo.json");
    fs::write(&media_file, b"fixture")?;
    fs::write(&danmaku_file, b"<i></i>")?;
    fs::write(&mediainfo_file, b"{}")?;
    fs::write(&other_danmaku_file, b"<i>keep</i>")?;
    fs::write(&other_mediainfo_file, b"{\"keep\":true}")?;
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
    assert!(!danmaku_file.exists());
    assert!(!mediainfo_file.exists());
    assert_eq!(fs::read(&other_danmaku_file)?, b"<i>keep</i>");
    assert_eq!(fs::read(&other_mediainfo_file)?, b"{\"keep\":true}");
    assert_no_staged_deletion_files(&root)?;
    Ok(())
}

#[tokio::test]
async fn deletion_cleans_sidecars_after_the_media_file_was_removed()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    fs::create_dir_all(&root)?;
    let media_file = root.join("Example.Movie.2024.strm");
    fs::write(&media_file, b"https://media.example.test/video.mkv\n")?;
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
    let sidecars = [
        "Example.Movie.2024.nfo",
        "Example.Movie.2024.zh.srt",
        "Example.Movie.2024.xml",
        "Example.Movie.2024-mediainfo.json",
    ];
    for sidecar in sidecars {
        fs::write(root.join(sidecar), b"sidecar fixture")?;
    }
    fs::remove_file(&media_file)?;

    let report = MediaDeleteService::new(database.clone())
        .delete(&item_id, Some(&source_id))
        .await?;

    assert_eq!(report.deleted_file_count, sidecars.len());
    for sidecar in sidecars {
        assert!(!root.join(sidecar).exists(), "{sidecar} was left behind");
    }
    let remaining_sources: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_sources WHERE id = ?")
            .bind(&source_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_sources, 0);
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
