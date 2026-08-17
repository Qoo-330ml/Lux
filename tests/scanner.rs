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
    assert_eq!(parsed.edition_name, None);
    assert_eq!(parsed.quality_label.as_deref(), Some("1080p"));
}

#[test]
fn movie_filename_parser_extracts_explicit_edition_and_quality() {
    let parsed =
        parse_movie_filename("Movie.Name.2020.Directors.Cut.2160p.WEB-DL.mkv").expect("movie name");

    assert_eq!(parsed.title, "Movie Name (Director's Cut)");
    assert_eq!(parsed.production_year, Some(2020));
    assert_eq!(parsed.edition_name.as_deref(), Some("Director's Cut"));
    assert_eq!(parsed.quality_label.as_deref(), Some("2160p"));
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
    let movie_dir = media_root.join("Example Movie (2020) [tmdbid=999]");
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
    let provider_ids: String = sqlx::query_scalar(
        "SELECT provider_ids_json FROM media_items WHERE item_type = 'MOVIE' LIMIT 1",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(provider_ids, r#"{"Tmdb":"999"}"#);

    let second = scanner.scan_movie_library(library.id).await?;
    assert_eq!(second.discovered_files, 1);
    assert_eq!(second.created_items, 0);
    assert_eq!(second.created_sources, 0);
    assert_eq!(second.skipped_files, 1);
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_chapters (
             id, media_source_id, start_position_ticks, marker_type,
             chapter_index, provider_id, confidence
         ) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind("detected-marker")
    .bind(&source_id)
    .bind(10_000_000_i64)
    .bind("INTRO_START")
    .bind(0_i64)
    .bind("org.lux.intro-outro-detector")
    .bind(0.9_f64)
    .execute(database.pool())
    .await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"changed-content").await?;
    let changed = scanner.scan_movie_library(library.id).await?;
    assert_eq!(changed.changed_files, 1);
    let remaining_markers: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_chapters WHERE media_source_id = ?")
            .bind(&source_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_markers, 0);
    let available_after_scan: i64 = sqlx::query_scalar(
        "SELECT has_available_source FROM media_items WHERE id = (
             SELECT item_id FROM media_sources LIMIT 1
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(available_after_scan, 1);

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
    let unavailable: i64 = sqlx::query_scalar(
        "SELECT has_available_source FROM media_items WHERE id = (
             SELECT item_id FROM media_sources LIMIT 1
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(unavailable, 0);

    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"fixture").await?;
    scanner.scan_movie_library(library.id).await?;
    let available_after_restore: i64 = sqlx::query_scalar(
        "SELECT has_available_source FROM media_items WHERE id = (
             SELECT item_id FROM media_sources LIMIT 1
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(available_after_restore, 1);

    let item_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE item_type <> 'FOLDER'")
            .fetch_one(database.pool())
            .await?;
    let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_sources")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(item_count, 1);
    assert_eq!(source_count, 1);
    let folder_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE item_type = 'FOLDER'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(folder_count, 1);
    database.close().await;

    let reopened = Database::connect(&config).await?;
    let persisted_item_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE item_type <> 'FOLDER'")
            .fetch_one(reopened.pool())
            .await?;
    assert_eq!(persisted_item_count, 1);
    Ok(())
}

#[tokio::test]
async fn scanner_recurses_through_nested_movie_directories()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    let movie_dir = media_root.join("Animation").join("Example Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2024.mkv"), b"fixture").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let report = LibraryScanner::new(database)
        .scan_movie_library(library.id)
        .await?;
    assert_eq!(report.discovered_files, 1);
    assert_eq!(report.created_items, 1);
    assert_eq!(report.created_sources, 1);
    Ok(())
}

#[tokio::test]
async fn scanner_aggregates_quality_sources_but_keeps_cuts_separate()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    for (name, bytes) in [
        ("Example.Movie.2024.1080p.mkv", b"1080".as_slice()),
        ("Example.Movie.2024.2160p.mkv", b"2160".as_slice()),
        (
            "Example.Movie.2024.Directors.Cut.1080p.mkv",
            b"directors".as_slice(),
        ),
    ] {
        tokio::fs::write(root.join(name), bytes).await?;
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
    assert_eq!(report.created_items, 2);
    assert_eq!(report.created_sources, 3);

    let items: Vec<(String, i64)> = sqlx::query_as(
        "SELECT mi.title, COUNT(ms.id)
         FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         GROUP BY mi.id
         ORDER BY mi.title",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        items,
        vec![
            ("Example Movie".to_owned(), 2),
            ("Example Movie (Director's Cut)".to_owned(), 1),
        ]
    );

    let versions: Vec<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT ms.edition_name, ms.quality_label
         FROM media_sources ms
         ORDER BY ms.edition_name IS NOT NULL, ms.quality_label DESC",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        versions,
        vec![
            (None, Some("2160p".to_owned())),
            (None, Some("1080p".to_owned())),
            (Some("Director's Cut".to_owned()), Some("1080p".to_owned())),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn unavailable_root_does_not_mark_entries_missing_and_recovers()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Safe Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    let movie_path = movie_dir.join("Safe.Movie.2020.mkv");
    tokio::fs::write(&movie_path, b"fixture").await?;
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
    tokio::fs::remove_dir_all(&root).await?;

    let unavailable = scanner.scan_movie_library(library.id).await?;
    assert_eq!(unavailable.unavailable_roots, 1);
    let state: (i64, i64) = sqlx::query_as(
        "SELECT lr.is_available,
                (SELECT is_missing FROM filesystem_entries LIMIT 1)
         FROM library_roots lr WHERE lr.library_id = ?",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(state, (0, 0));

    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(&movie_path, b"fixture").await?;
    let recovered = scanner.scan_movie_library(library.id).await?;
    assert_eq!(recovered.unavailable_roots, 0);
    let available: i64 =
        sqlx::query_scalar("SELECT is_available FROM library_roots WHERE library_id = ?")
            .bind(library.id.to_string())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(available, 1);
    Ok(())
}

#[tokio::test]
async fn scanner_can_process_one_directory_without_marking_other_entries_missing()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let first_directory = root.join("First");
    let second_directory = root.join("Second");
    tokio::fs::create_dir_all(&first_directory).await?;
    tokio::fs::create_dir_all(&second_directory).await?;
    tokio::fs::write(first_directory.join("First.Movie.2020.mkv"), b"first").await?;
    tokio::fs::write(second_directory.join("Second.Movie.2021.mkv"), b"second").await?;

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

    tokio::fs::write(first_directory.join("Added.Movie.2022.mkv"), b"added").await?;
    let incremental = scanner
        .scan_movie_directory(library.id, &first_directory)
        .await?;
    assert_eq!(incremental.discovered_files, 2);
    assert_eq!(incremental.created_items, 1);
    assert_eq!(incremental.created_sources, 1);
    assert_eq!(incremental.skipped_files, 1);
    assert_eq!(incremental.marked_missing, 0);

    let second_missing: i64 = sqlx::query_scalar(
        "SELECT is_missing FROM filesystem_entries
         WHERE relative_path LIKE 'Second/%'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(second_missing, 0);

    let outside = temp_dir.path().join("Outside");
    tokio::fs::create_dir(&outside).await?;
    let outside_error = scanner
        .scan_movie_directory(library.id, &outside)
        .await
        .expect_err("directory outside the library root");
    assert!(matches!(
        outside_error,
        luxd::application::scanner::ScannerError::InvalidRelativePath(_)
    ));
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
    assert_eq!(database.schema_version().await?, 67);
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
    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_media_items_parent_removed',
             'idx_media_items_series_removed'
         )
         ORDER BY name",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        indexes,
        [
            "idx_media_items_parent_removed",
            "idx_media_items_series_removed"
        ]
    );
    Ok(())
}
