use luxd::{
    application::{
        libraries::LibraryService,
        scanner::{LibraryScanner, ScanJobService, compute_file_fingerprint, parse_movie_filename},
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
    let removed_at: Option<i64> = sqlx::query_scalar(
        "SELECT removed_at FROM media_items WHERE id = (
             SELECT item_id FROM media_sources LIMIT 1
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(removed_at.is_some());

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
    let restored_removed_at: Option<i64> = sqlx::query_scalar(
        "SELECT removed_at FROM media_items WHERE id = (
             SELECT item_id FROM media_sources LIMIT 1
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(restored_removed_at.is_none());

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
async fn movie_scan_groups_chinese_source_variants_into_one_item()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("ABF-301 (118abf301)-有码-C.mp4"), b"watermarked").await?;
    tokio::fs::write(root.join("ABF-301 (118abf301)-破解-C.mp4"), b"cracked").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;

    let report = LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    assert_eq!(report.created_items, 1);
    assert_eq!(report.created_sources, 2);

    let item_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
         WHERE library_id = ? AND item_type = 'MOVIE' AND removed_at IS NULL",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(item_count, 1);

    let editions: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT ms.edition_name
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         ORDER BY fe.relative_path",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        editions,
        vec![Some("有码 C".to_owned()), Some("破解 C".to_owned())]
    );
    Ok(())
}

#[tokio::test]
async fn movie_full_scan_groups_generic_trailing_suffixes_with_sibling_bases()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let media_root = temp_dir.path().join("Movies");
    for code in ["ADN-610", "ADN-723"] {
        let movie_dir = media_root.join(code);
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(movie_dir.join(format!("{code}.mp4")), b"base").await?;
        tokio::fs::write(movie_dir.join(format!("{code}-C.mp4")), b"variant").await?;
    }
    let alternate_dir = media_root.join("ADN-725");
    tokio::fs::create_dir_all(&alternate_dir).await?;
    tokio::fs::write(alternate_dir.join("ADN-725.MKV"), b"base").await?;
    tokio::fs::write(
        alternate_dir.join("ADN-725-Alternate-Cut.mp4"),
        b"alternate version",
    )
    .await?;
    let unpaired_dir = media_root.join("Unpaired");
    tokio::fs::create_dir_all(&unpaired_dir).await?;
    tokio::fs::write(unpaired_dir.join("ADN-999-Alternate.mp4"), b"unpaired").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF-8 media root")?,
        )
        .await?
        .root;
    let jobs = ScanJobService::new(database.clone());

    let job = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&job.id, 100, None).await?;
    let movies: Vec<(String, i64)> = sqlx::query_as(
        "SELECT item.title, COUNT(source.id)
         FROM media_items item
         JOIN media_sources source ON source.item_id = item.id
         WHERE item.library_id = ? AND item.item_type = 'MOVIE' AND item.removed_at IS NULL
         GROUP BY item.id
         ORDER BY item.title",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        movies,
        vec![
            ("ADN 610".to_owned(), 2),
            ("ADN 723".to_owned(), 2),
            ("ADN 725".to_owned(), 2),
            ("ADN 999 Alternate".to_owned(), 1),
        ]
    );

    let editions: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT entry.relative_path, source.edition_name
         FROM media_sources source
         JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
         ORDER BY entry.relative_path",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        editions,
        vec![
            ("ADN-610/ADN-610-C.mp4".to_owned(), Some("C".to_owned())),
            ("ADN-610/ADN-610.mp4".to_owned(), None),
            ("ADN-723/ADN-723-C.mp4".to_owned(), Some("C".to_owned())),
            ("ADN-723/ADN-723.mp4".to_owned(), None),
            (
                "ADN-725/ADN-725-Alternate-Cut.mp4".to_owned(),
                Some("Alternate-Cut".to_owned()),
            ),
            ("ADN-725/ADN-725.MKV".to_owned(), None),
            ("Unpaired/ADN-999-Alternate.mp4".to_owned(), None),
        ]
    );

    for code in ["ADN-610", "ADN-723"] {
        let legacy_item_id = format!("legacy-{code}");
        let legacy_title = format!("{} C", code.replace('-', " "));
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, original_title,
                identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(&legacy_item_id)
        .bind(library.id.to_string())
        .bind(&legacy_title)
        .bind(legacy_title.to_lowercase())
        .bind(&legacy_title)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "UPDATE media_sources SET item_id = ?, edition_name = NULL
             WHERE filesystem_entry_id = (
                 SELECT id FROM filesystem_entries
                 WHERE library_root_id = ? AND relative_path = ?
             )",
        )
        .bind(&legacy_item_id)
        .bind(root.id.to_string())
        .bind(format!("{code}/{code}-C.mp4"))
        .execute(database.pool())
        .await?;
    }

    let repair = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&repair.id, 100, None).await?;
    let removed_legacy_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
         WHERE id IN (?, ?) AND removed_at IS NOT NULL",
    )
    .bind("legacy-ADN-610")
    .bind("legacy-ADN-723")
    .fetch_one(database.pool())
    .await?;
    assert_eq!(removed_legacy_items, 2);

    let alternate_item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'MOVIE' AND title = 'ADN 725'",
    )
    .fetch_one(database.pool())
    .await?;
    sqlx::query(
        "UPDATE media_items SET title = 'Metadata Title', sort_title = 'metadata title',
               production_year = 2024 WHERE id = ?",
    )
    .bind(&alternate_item_id)
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE media_sources SET probe_status = 'READY'")
        .execute(database.pool())
        .await?;
    let repeated = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&repeated.id, 100, None).await?;
    let probe_statuses: Vec<String> =
        sqlx::query_scalar("SELECT probe_status FROM media_sources ORDER BY id")
            .fetch_all(database.pool())
            .await?;
    assert_eq!(probe_statuses, vec!["READY".to_owned(); 7]);
    let alternate_sources: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_sources WHERE item_id = ?")
            .bind(&alternate_item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        alternate_sources, 2,
        "generic suffix with a different base extension"
    );
    Ok(())
}

#[tokio::test]
async fn movie_rescan_repairs_chinese_source_variants_split_by_an_older_scan()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("ABF-301 (118abf301)-有码-C.mp4"), b"watermarked").await?;
    tokio::fs::write(root.join("ABF-301 (118abf301)-破解-C.mp4"), b"cracked").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(library.id).await?;

    let original_item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items
         WHERE library_id = ? AND item_type = 'MOVIE' AND removed_at IS NULL",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let cracked_source_id: String = sqlx::query_scalar(
        "SELECT ms.id
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path LIKE '%破解%'",
    )
    .fetch_one(database.pool())
    .await?;
    sqlx::query(
        "UPDATE media_items
         SET title = 'ABF 301 118abf301 有码 C', sort_title = 'abf 301 118abf301 有码 c'
         WHERE id = ?",
    )
    .bind(&original_item_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, original_title,
             identification_status
         ) VALUES (?, ?, 'MOVIE', ?, ?, ?, 'LOCAL_CONFIRMED')",
    )
    .bind("legacy-cracked-version")
    .bind(library.id.to_string())
    .bind("ABF 301 118abf301 破解 C")
    .bind("abf 301 118abf301 破解 c")
    .bind("ABF 301 118abf301 破解 C")
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE media_sources SET item_id = ? WHERE id = ?")
        .bind("legacy-cracked-version")
        .bind(&cracked_source_id)
        .execute(database.pool())
        .await?;

    scanner.scan_movie_library(library.id).await?;

    let active_item_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
         WHERE library_id = ? AND item_type = 'MOVIE' AND removed_at IS NULL",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(active_item_count, 1);
    let source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources
         WHERE item_id = (
             SELECT id FROM media_items
             WHERE library_id = ? AND item_type = 'MOVIE' AND removed_at IS NULL
         )",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(source_count, 2);
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

    let scanner = LibraryScanner::new(database.clone());
    let report = scanner.scan_movie_library(library.id).await?;
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

    tokio::fs::remove_file(root.join("Example.Movie.2024.2160p.mkv")).await?;
    scanner.scan_movie_library(library.id).await?;
    let removed_at: Option<i64> = sqlx::query_scalar(
        "SELECT removed_at FROM media_items
         WHERE title = 'Example Movie' LIMIT 1",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(
        removed_at.is_none(),
        "a movie with another available source must stay active"
    );
    Ok(())
}

#[tokio::test]
async fn movie_scan_reuses_removed_item_after_path_move_without_inode()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let old_path = root.join("Old/Example.Movie.2020.mkv");
    tokio::fs::create_dir_all(old_path.parent().ok_or("missing parent")?).await?;
    tokio::fs::write(&old_path, b"fixture").await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(library.id).await?;
    let original_item_id: String =
        sqlx::query_scalar("SELECT item_id FROM media_sources WHERE source_kind = 'LOCAL_FILE'")
            .fetch_one(database.pool())
            .await?;

    tokio::fs::remove_file(&old_path).await?;
    scanner.scan_movie_library(library.id).await?;
    let removed_at: Option<i64> =
        sqlx::query_scalar("SELECT removed_at FROM media_items WHERE id = ?")
            .bind(&original_item_id)
            .fetch_one(database.pool())
            .await?;
    assert!(removed_at.is_some());
    sqlx::query("UPDATE filesystem_entries SET inode = NULL WHERE relative_path = ?")
        .bind("Old/Example.Movie.2020.mkv")
        .execute(database.pool())
        .await?;

    let new_path = root.join("New/Example.Movie.2020.mkv");
    tokio::fs::create_dir_all(new_path.parent().ok_or("missing parent")?).await?;
    tokio::fs::write(&new_path, b"fixture").await?;
    scanner.scan_movie_library(library.id).await?;

    let item_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'MOVIE' AND library_id = ?",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(item_ids, vec![original_item_id.clone()]);
    let restored_removed_at: Option<i64> =
        sqlx::query_scalar("SELECT removed_at FROM media_items WHERE id = ?")
            .bind(original_item_id)
            .fetch_one(database.pool())
            .await?;
    assert!(restored_removed_at.is_none());
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
    let removed_at: Option<i64> =
        sqlx::query_scalar("SELECT removed_at FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    assert!(removed_at.is_none());

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
async fn targeted_movie_scan_batches_files_across_directory_batches()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let batch_dir = root.join("Batch");
    tokio::fs::create_dir_all(&batch_dir).await?;
    for index in 0..501 {
        tokio::fs::write(
            batch_dir.join(format!("Batch.Movie.Title{index:03}.2024.mkv")),
            b"fixture",
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

    let report = LibraryScanner::new(database)
        .scan_movie_directory(library.id, &batch_dir)
        .await?;
    assert_eq!(report.discovered_files, 501);
    assert_eq!(report.created_items, 501);
    assert_eq!(report.created_sources, 501);
    assert_eq!(report.skipped_files, 0);
    Ok(())
}

#[tokio::test]
async fn movie_rescan_keeps_merged_title_variants_on_the_primary_item()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let primary_dir = root.join("Primary");
    let secondary_dir = root.join("Alternate");
    tokio::fs::create_dir_all(&primary_dir).await?;
    tokio::fs::create_dir_all(&secondary_dir).await?;
    tokio::fs::write(primary_dir.join("Primary.Movie.2020.mkv"), b"primary").await?;
    tokio::fs::write(secondary_dir.join("Alternate.Movie.2020.mkv"), b"alternate").await?;

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

    let primary_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'MOVIE' AND title = 'Primary Movie'",
    )
    .fetch_one(database.pool())
    .await?;
    let secondary_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'MOVIE' AND title = 'Alternate Movie'",
    )
    .fetch_one(database.pool())
    .await?;
    sqlx::query("UPDATE media_items SET merged_into_item_id = ? WHERE id = ?")
        .bind(&primary_id)
        .bind(&secondary_id)
        .execute(database.pool())
        .await?;
    tokio::fs::write(
        secondary_dir.join("Alternate.Movie.2020.4K.mkv"),
        b"alternate-4k",
    )
    .await?;

    let report = scanner
        .scan_movie_directory(library.id, &secondary_dir)
        .await?;
    assert_eq!(report.created_items, 0);
    assert_eq!(report.created_sources, 1);
    assert_eq!(report.changed_files, 0);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_sources WHERE item_id = ?",)
            .bind(&primary_id)
            .fetch_one(database.pool())
            .await?,
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM media_sources WHERE item_id = ?",)
            .bind(&secondary_id)
            .fetch_one(database.pool())
            .await?,
        1
    );
    let new_source_item_id: String = sqlx::query_scalar(
        "SELECT ms.item_id
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path = 'Alternate/Alternate.Movie.2020.4K.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(new_source_item_id, primary_id);
    Ok(())
}

#[tokio::test]
async fn series_rescan_routes_new_episodes_to_the_primary_series_after_merge()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    let primary_dir = root.join("Primary Show").join("Season 01");
    let secondary_dir = root.join("Secondary Show").join("Season 01");
    tokio::fs::create_dir_all(&primary_dir).await?;
    tokio::fs::create_dir_all(&secondary_dir).await?;
    tokio::fs::write(primary_dir.join("Primary.Show.S01E01.mkv"), b"primary").await?;
    tokio::fs::write(
        secondary_dir.join("Secondary.Show.S01E01.mkv"),
        b"secondary",
    )
    .await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Mixed, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_mixed_library(library.id).await?;

    let primary_series: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'SERIES' AND title = 'Primary Show'",
    )
    .fetch_one(database.pool())
    .await?;
    let secondary_series: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'SERIES' AND title = 'Secondary Show'",
    )
    .fetch_one(database.pool())
    .await?;
    let primary_season: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'SEASON' AND series_id = ?",
    )
    .bind(&primary_series)
    .fetch_one(database.pool())
    .await?;
    let secondary_season: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'SEASON' AND series_id = ?",
    )
    .bind(&secondary_series)
    .fetch_one(database.pool())
    .await?;
    sqlx::query("UPDATE media_items SET merged_into_item_id = ? WHERE id = ?")
        .bind(&primary_series)
        .bind(&secondary_series)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET merged_into_item_id = ? WHERE id = ?")
        .bind(&primary_season)
        .bind(&secondary_season)
        .execute(database.pool())
        .await?;
    tokio::fs::write(
        secondary_dir.join("Secondary.Show.S01E02.mkv"),
        b"secondary-new-episode",
    )
    .await?;

    let report = scanner.scan_mixed_library(library.id).await?;
    assert_eq!(report.created_items, 1);
    let new_episode: (String, String) = sqlx::query_as(
        "SELECT parent_id, series_id
         FROM media_items
         WHERE item_type = 'EPISODE' AND episode_number = 2",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(new_episode, (primary_season, primary_series));
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
    assert_eq!(database.schema_version().await?, 168);
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
