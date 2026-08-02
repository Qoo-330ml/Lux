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
    assert_eq!(first.created_items, 11);
    assert_eq!(first.created_sources, 5);
    assert_eq!(first.skipped_files, 0);

    let hierarchy: Vec<HierarchyRow> = sqlx::query_as(
        "SELECT item_type, parent_id, series_id, season_number, episode_number, title, identity_key
             FROM media_items ORDER BY item_type, identity_key",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(hierarchy.len(), 11);
    assert_eq!(
        hierarchy
            .iter()
            .filter(|row| row.item_type == "SERIES")
            .count(),
        2
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
        2
    );
    assert!(
        hierarchy
            .iter()
            .any(|row| row.item_type == "SEASON" && row.season_number == Some(3))
    );
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
    assert_eq!(second.skipped_files, 5);
    let ids_after: Vec<(String, String)> =
        sqlx::query_as("SELECT identity_key, id FROM media_items ORDER BY identity_key")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(ids_before, ids_after);
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
    identity_key: String,
}
