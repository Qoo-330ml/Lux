use luxd::{
    application::{
        libraries::LibraryService,
        scanner::{LibraryScanner, parse_episode_filename},
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[test]
fn episode_filename_parser_handles_seasons_episodes_and_specials() {
    let cases = [
        ("Show.Name.S01E02.mkv", Some((1, 2))),
        ("Show Name - 2x07 - Title.mp4", Some((2, 7))),
        ("Show.Name.S00E03.mkv", Some((0, 3))),
        ("not-an-episode.mkv", None),
    ];
    for (filename, expected) in cases {
        let parsed = parse_episode_filename(filename);
        assert_eq!(
            parsed.as_ref().map(|value| (value.season, value.episode)),
            expected,
            "{filename}"
        );
    }
}

#[tokio::test]
async fn series_scan_builds_stable_series_season_episode_hierarchy()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    create_file(&root.join("Example Show/Season 01/Example.Show.S01E01.mkv")).await?;
    create_file(&root.join("Example Show/Season 01/Example.Show.S01E02.mkv")).await?;
    create_file(&root.join("Example Show/Specials/Example.Show.S00E01.mkv")).await?;
    create_file(&root.join("Example Show/Season 03/Example.Show.S03E01.mkv")).await?;
    create_file(&root.join("Another Show/Another.Show.S01E01.mkv")).await?;
    create_file(&root.join("Drama/Grouped Show (2024)/Season 1/Grouped.Show.S01E01.mkv")).await?;
    create_file(&root.join("Drama/Grouped Show (2024)/Season 1(1)/Grouped.Show.S01E02.mkv"))
        .await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());

    let first = scanner.scan_series_library(library.id).await?;
    assert_eq!(first.created_items, 15);
    assert_eq!(first.created_sources, 7);
    assert_eq!(first.skipped_files, 0);

    let hierarchy: Vec<HierarchyRow> = sqlx::query_as(
        "SELECT item_type, parent_id, series_id, season_number, episode_number, title, production_year, identity_key
             FROM media_items ORDER BY item_type, identity_key",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(hierarchy.len(), 15);
    assert_eq!(
        hierarchy
            .iter()
            .filter(|row| row.item_type == "SERIES")
            .count(),
        3
    );
    assert_eq!(
        hierarchy
            .iter()
            .filter(|row| row.item_type == "SEASON" && row.season_number == Some(0))
            .count(),
        1
    );
    assert_eq!(
        hierarchy
            .iter()
            .filter(|row| row.item_type == "SEASON" && row.season_number == Some(1))
            .count(),
        3
    );
    assert!(
        hierarchy
            .iter()
            .any(|row| row.item_type == "SEASON" && row.season_number == Some(3))
    );
    assert!(hierarchy.iter().any(|row| {
        row.item_type == "SERIES"
            && row.title == "Grouped Show"
            && row.production_year == Some(2024)
    }));
    assert!(
        hierarchy
            .iter()
            .filter(|row| row.item_type == "EPISODE" && row.season_number == Some(0))
            .all(|row| row.episode_number == Some(1))
    );
    for row in hierarchy.iter().filter(|row| row.item_type == "EPISODE") {
        assert!(row.parent_id.is_some());
        assert!(row.series_id.is_some());
        assert!(row.identity_key.starts_with("episode:"));
    }

    let ids_before: Vec<(String, String)> =
        sqlx::query_as("SELECT identity_key, id FROM media_items ORDER BY identity_key")
            .fetch_all(database.pool())
            .await?;
    let second = scanner.scan_series_library(library.id).await?;
    assert_eq!(second.created_items, 0);
    assert_eq!(second.created_sources, 0);
    assert_eq!(second.skipped_files, 7);
    let ids_after: Vec<(String, String)> =
        sqlx::query_as("SELECT identity_key, id FROM media_items ORDER BY identity_key")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(ids_before, ids_after);
    Ok(())
}

#[tokio::test]
async fn series_scan_repairs_existing_grouped_hierarchy() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    let episode_path = root.join("Drama/Grouped Show (2024)/Season 1/Grouped.Show.S01E01.mkv");
    create_file(&episode_path).await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_series_library(library.id).await?;

    let root_id: String = sqlx::query_scalar("SELECT id FROM library_roots WHERE library_id = ?")
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?;
    let expected_episode_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE identity_key = ? AND removed_at IS NULL",
    )
    .bind(format!(
        "episode:{}:{}:season:{}:episode:{}:edition:{}",
        root_id.clone(),
        "Drama/Grouped Show (2024)",
        1,
        1,
        "standard"
    ))
    .fetch_one(database.pool())
    .await?;
    let old_series_id = "legacy-series";
    let old_season_id = "legacy-season";
    let old_episode_id = "legacy-episode";
    sqlx::query(
        "INSERT INTO media_items (id, library_id, item_type, title, sort_title, identification_status, identity_key)
         VALUES (?, ?, 'SERIES', 'Drama', 'drama', 'LOCAL_CONFIRMED', 'legacy:series')",
    )
    .bind(old_series_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_items (id, library_id, item_type, parent_id, series_id, season_number, title, sort_title, identification_status, identity_key)
         VALUES (?, ?, 'SEASON', ?, ?, 1, 'Season 01', 'season 01', 'LOCAL_CONFIRMED', 'legacy:season')",
    )
    .bind(old_season_id)
    .bind(library.id.to_string())
    .bind(old_series_id)
    .bind(old_series_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_items (id, library_id, item_type, parent_id, series_id, season_number, episode_number, title, sort_title, identification_status, identity_key)
         VALUES (?, ?, 'EPISODE', ?, ?, 1, 1, 'Legacy Episode', 'legacy episode', 'LOCAL_CONFIRMED', 'legacy:episode')",
    )
    .bind(old_episode_id)
    .bind(library.id.to_string())
    .bind(old_season_id)
    .bind(old_series_id)
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE media_sources SET item_id = ? WHERE item_id = ?")
        .bind(old_episode_id)
        .bind(&expected_episode_id)
        .execute(database.pool())
        .await?;

    let repaired = scanner.scan_series_library(library.id).await?;
    assert_eq!(repaired.created_items, 0);
    assert_eq!(repaired.skipped_files, 1);
    let source_item_id: String = sqlx::query_scalar(
        "SELECT ms.item_id
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.library_root_id = ? AND fe.relative_path = ?",
    )
    .bind(root_id)
    .bind("Drama/Grouped Show (2024)/Season 1/Grouped.Show.S01E01.mkv")
    .fetch_one(database.pool())
    .await?;
    assert_eq!(source_item_id, expected_episode_id);
    let old_active_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items WHERE id IN (?, ?, ?) AND removed_at IS NULL",
    )
    .bind(old_series_id)
    .bind(old_season_id)
    .bind(old_episode_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(old_active_count, 0);
    Ok(())
}

#[tokio::test]
async fn series_scan_groups_episode_versions_into_one_item_and_labels_sources()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    create_file(&root.join("Example Show/Season 01/Example.Show.S01E01.1080p.SDR.mkv")).await?;
    create_file(&root.join("Example Show/Season 01/Example.Show.S01E01.2160p.HDR.mkv")).await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let report = LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    assert_eq!(report.created_items, 3);
    assert_eq!(report.created_sources, 2);

    let item_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items WHERE item_type = 'EPISODE' AND removed_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(item_count, 1);

    let sources: Vec<SourceRow> = sqlx::query_as(
        "SELECT quality_label, is_default
         FROM media_sources
         WHERE item_id = (SELECT id FROM media_items WHERE item_type = 'EPISODE' AND removed_at IS NULL)
         ORDER BY quality_label",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].quality_label.as_deref(), Some("1080p SDR"));
    assert_eq!(sources[1].quality_label.as_deref(), Some("2160p HDR"));
    assert_eq!(
        sources
            .iter()
            .filter(|source| source.is_default != 0)
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn series_scan_allows_same_title_and_year_in_different_directories()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    create_file(
        &root.join("中国动漫/2010/果宝特攻 (2010) {tmdb-118968}/Season 1/果宝特攻-S01E18-720p.mkv"),
    )
    .await?;
    create_file(
        &root.join("儿童动画/2010/果宝特攻 (2010) {tmdb-118968}/Season 1/果宝特攻-S01E18-480p.mkv"),
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

    let report = LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    assert_eq!(report.created_sources, 2);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM media_items
             WHERE library_id = ? AND item_type = 'SERIES' AND removed_at IS NULL",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?,
        2
    );
    Ok(())
}

#[tokio::test]
async fn series_scan_repairs_legacy_identity_keys_without_creating_duplicates()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    let episode_path = root.join("Example Show (2024)/Season 01/Example.Show.S01E01.mkv");
    create_file(&episode_path).await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_series_library(library.id).await?;

    sqlx::query(
        "UPDATE media_items
         SET identity_key = CASE item_type
             WHEN 'SERIES' THEN NULL
             WHEN 'SEASON' THEN 'legacy:season'
             WHEN 'EPISODE' THEN 'episode:legacy:relative/path'
         END
         WHERE library_id = ?",
    )
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    assert_eq!(scanner.repair_legacy_identity_keys().await?, 1);
    assert_eq!(scanner.repair_legacy_identity_keys().await?, 0);
    let second = scanner.scan_series_library(library.id).await?;
    assert_eq!(second.created_items, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM media_items WHERE library_id = ? AND removed_at IS NULL",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?,
        3
    );
    Ok(())
}

#[tokio::test]
async fn readding_a_deleted_root_reuses_existing_series_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    create_file(&root.join("Example Show (2024)/Season 01/Example.Show.S01E01.mkv")).await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root_record = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?
        .root;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_series_library(library.id).await?;
    let before: Vec<(String, String)> = sqlx::query_as(
        "SELECT item_type, id FROM media_items WHERE library_id = ? ORDER BY item_type",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;

    sqlx::query(
        "UPDATE media_items
         SET identity_key = CASE item_type
             WHEN 'SERIES' THEN ?
             WHEN 'SEASON' THEN ?
             WHEN 'EPISODE' THEN ?
         END
         WHERE library_id = ?",
    )
    .bind(format!("series:{}:Example Show (2024)", root_record.id))
    .bind(format!(
        "series:{}:Example Show (2024):season:1",
        root_record.id
    ))
    .bind(format!(
        "episode:{}:Example Show (2024)/Season 01/Example.Show.S01E01.mkv",
        root_record.id
    ))
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    libraries.delete_root(library.id, root_record.id).await?;
    let restored = libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?
        .root;
    assert_eq!(restored.id, root_record.id);

    let second = scanner.scan_series_library(library.id).await?;
    assert_eq!(second.created_items, 0);
    let after: Vec<(String, String)> = sqlx::query_as(
        "SELECT item_type, id FROM media_items WHERE library_id = ? ORDER BY item_type",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(after, before);
    Ok(())
}

#[tokio::test]
async fn series_scan_preserves_items_when_episode_directory_moves_by_inode()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    let old_episode = root.join("Updating Show/Season 01/Updating.Show.S01E01.mkv");
    create_file(&old_episode).await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_series_library(library.id).await?;
    let before: Vec<(String, String)> = sqlx::query_as(
        "SELECT item_type, id FROM media_items WHERE library_id = ? ORDER BY item_type",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;

    let new_directory = root.join("Completed Show/Season 01");
    tokio::fs::create_dir_all(&new_directory).await?;
    tokio::fs::rename(
        &old_episode,
        new_directory.join("Completed.Show.S01E01.mkv"),
    )
    .await?;
    tokio::fs::remove_dir_all(root.join("Updating Show")).await?;

    let second = scanner.scan_series_library(library.id).await?;
    assert_eq!(second.created_items, 0);
    let after: Vec<(String, String)> = sqlx::query_as(
        "SELECT item_type, id FROM media_items WHERE library_id = ? ORDER BY item_type",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(after, before);
    Ok(())
}

#[tokio::test]
async fn series_scan_preserves_items_when_file_moves_between_roots()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let first_root = temp_dir.path().join("A");
    let second_root = temp_dir.path().join("B");
    let old_episode = first_root.join("Moving Show/Season 01/Moving.Show.S01E01.mkv");
    create_file(&old_episode).await?;
    tokio::fs::create_dir_all(&second_root).await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, first_root.to_str().ok_or("non-utf8 root")?)
        .await?;
    libraries
        .add_root(library.id, second_root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_series_library(library.id).await?;
    let before: Vec<(String, String)> = sqlx::query_as(
        "SELECT item_type, id FROM media_items WHERE library_id = ? ORDER BY item_type",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;

    let new_directory = second_root.join("Moved Show/Season 01");
    tokio::fs::create_dir_all(&new_directory).await?;
    tokio::fs::rename(&old_episode, new_directory.join("Moved.Show.S01E01.mkv")).await?;
    tokio::fs::remove_dir_all(first_root.join("Moving Show")).await?;

    let second = scanner.scan_series_library(library.id).await?;
    assert_eq!(second.created_items, 0);
    let after: Vec<(String, String)> = sqlx::query_as(
        "SELECT item_type, id FROM media_items WHERE library_id = ? ORDER BY item_type",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(after, before);
    Ok(())
}

async fn create_file(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, b"episode").await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct HierarchyRow {
    item_type: String,
    parent_id: Option<String>,
    series_id: Option<String>,
    season_number: Option<i64>,
    episode_number: Option<i64>,
    title: String,
    production_year: Option<i64>,
    identity_key: String,
}

#[derive(sqlx::FromRow)]
struct SourceRow {
    quality_label: Option<String>,
    is_default: i64,
}
