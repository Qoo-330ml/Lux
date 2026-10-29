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

#[tokio::test]
async fn movie_rescan_repairs_cd_parts_that_were_split_by_an_older_scan()
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
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(library.id).await?;

    let initial_item_id: String = sqlx::query_scalar("SELECT id FROM media_items")
        .fetch_one(database.pool())
        .await?;
    let source_ids: Vec<String> = sqlx::query_scalar(
        "SELECT ms.id
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         ORDER BY fe.relative_path",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(source_ids.len(), 3);

    sqlx::query(
        "UPDATE media_items
         SET title = 'FC22378556无码 cd1', sort_title = 'fc22378556无码 cd1'
         WHERE id = ?",
    )
    .bind(&initial_item_id)
    .execute(database.pool())
    .await?;
    for (item_id, part) in [("legacy-cd2", 2), ("legacy-cd3", 3)] {
        let title = format!("FC22378556无码 cd{part}");
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, original_title,
                 identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(library.id.to_string())
        .bind(&title)
        .bind(title.to_lowercase())
        .bind(&title)
        .execute(database.pool())
        .await?;
    }
    for (source_id, item_id) in [
        (&source_ids[1], "legacy-cd2"),
        (&source_ids[2], "legacy-cd3"),
    ] {
        sqlx::query("UPDATE media_sources SET item_id = ? WHERE id = ?")
            .bind(item_id)
            .bind(source_id)
            .execute(database.pool())
            .await?;
    }

    scanner.scan_movie_library(library.id).await?;

    let active_item_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
         WHERE library_id = ? AND item_type = 'MOVIE' AND removed_at IS NULL",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let merged_source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources
         WHERE item_id = (
             SELECT id FROM media_items
             WHERE library_id = ? AND title = 'FC22378556无码' AND removed_at IS NULL
         )",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(active_item_count, 1);
    assert_eq!(merged_source_count, 3);
    Ok(())
}
