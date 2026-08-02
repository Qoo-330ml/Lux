use luxd::{
    application::{
        libraries::LibraryService,
        scanner::{LibraryScanner, compute_file_fingerprint, parse_movie_filename},
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[test]
fn movie_filename_parser_handles_year_and_quality_suffix() {
    let parsed = parse_movie_filename("Movie.Name.2020.1080p.mkv").expect("movie name");

    assert_eq!(parsed.title, "Movie Name");
    assert_eq!(parsed.production_year, Some(2020));
}

#[test]
fn movie_filename_parser_preserves_title_without_year() {
    let parsed = parse_movie_filename("A Film Without Year.MP4").expect("movie name");

    assert_eq!(parsed.title, "A Film Without Year");
    assert_eq!(parsed.production_year, None);
}

#[test]
fn file_fingerprint_is_stable_and_changes_when_inputs_change() {
    let first = compute_file_fingerprint("Movies/A.mkv", 10, 20, Some(1), Some(2));
    assert_eq!(
        first,
        compute_file_fingerprint("Movies/A.mkv", 10, 20, Some(1), Some(2))
    );
    assert_ne!(
        first,
        compute_file_fingerprint("Movies/A.mkv", 11, 20, Some(1), Some(2))
    );
    assert_ne!(
        first,
        compute_file_fingerprint("Movies/B.mkv", 10, 20, Some(1), Some(2))
    );
    assert_ne!(
        first,
        compute_file_fingerprint("Movies/A.mkv", 10, 21, Some(1), Some(2))
    );
}

#[tokio::test]
async fn scanner_discovers_one_movie_and_is_idempotent_after_restart()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"fixture").await?;
    tokio::fs::write(movie_dir.join("ignore.txt"), b"ignore").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let scanner = LibraryScanner::new(database.clone());
    let first = scanner.scan_movie_library(library.id).await?;
    assert_eq!(first.discovered_files, 1);
    assert_eq!(first.created_items, 1);
    assert_eq!(first.created_sources, 1);
    assert_eq!(first.skipped_files, 0);

    let second = scanner.scan_movie_library(library.id).await?;
    assert_eq!(second.discovered_files, 1);
    assert_eq!(second.created_items, 0);
    assert_eq!(second.created_sources, 0);
    assert_eq!(second.skipped_files, 1);

    tokio::fs::remove_file(movie_dir.join("Example.Movie.2020.mkv")).await?;
    let third = scanner.scan_movie_library(library.id).await?;
    assert_eq!(third.discovered_files, 0);
    assert_eq!(third.marked_missing, 1);
    let missing: i64 = sqlx::query_scalar(
        "SELECT is_missing FROM filesystem_entries WHERE relative_path LIKE '%Example.Movie.2020.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(missing, 1);

    let item_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_items")
        .fetch_one(database.pool())
        .await?;
    let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_sources")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(item_count, 1);
    assert_eq!(source_count, 1);
    database.close().await;

    let reopened = Database::connect(&config).await?;
    let persisted_item_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_items")
        .fetch_one(reopened.pool())
        .await?;
    assert_eq!(persisted_item_count, 1);
    Ok(())
}

#[tokio::test]
async fn media_catalog_migration_creates_expected_tables() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    assert_eq!(database.schema_version().await?, 10);
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name IN ('filesystem_entries', 'media_items', 'media_sources', 'media_streams')
         ORDER BY name",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        tables,
        [
            "filesystem_entries",
            "media_items",
            "media_sources",
            "media_streams"
        ]
    );
    Ok(())
}
