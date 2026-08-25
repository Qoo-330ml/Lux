use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use luxd::{
    application::libraries::{
        LibraryService, LibraryServiceError, LibrarySettingsPatch, LibraryWarningCode,
    },
    config::Config,
    library::{
        LibraryKind, LibraryScraper, LibraryScraperRole, RootOverlap, classify_root_overlap,
        inspect_root_path,
    },
    storage::Database,
};

#[tokio::test]
async fn inspect_root_path_reports_canonical_readable_and_writable_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let media_dir = temp_dir.path().join("Movies");
    tokio::fs::create_dir(&media_dir).await?;

    let inspection = inspect_root_path(&media_dir).await?;

    assert_eq!(inspection.canonical_path, media_dir.canonicalize()?);
    assert!(inspection.is_available);
    assert!(inspection.is_readable);
    assert!(inspection.is_writable);
    Ok(())
}

#[tokio::test]
async fn missing_root_path_is_rejected_without_creating_it() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let missing = temp_dir.path().join("does-not-exist");

    let error = inspect_root_path(&missing).await.expect_err("missing path");

    assert!(error.is_unavailable());
    assert!(!missing.exists());
}

#[test]
fn root_overlap_distinguishes_exact_nested_and_disjoint_paths() {
    assert_eq!(
        classify_root_overlap(Path::new("/media"), Path::new("/media")),
        RootOverlap::Exact
    );
    assert_eq!(
        classify_root_overlap(Path::new("/media"), Path::new("/media/movies")),
        RootOverlap::Nested
    );
    assert_eq!(
        classify_root_overlap(Path::new("/media"), Path::new("/other")),
        RootOverlap::Disjoint
    );
}

#[tokio::test]
async fn library_migration_creates_libraries_and_roots_tables()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };

    let database = Database::connect(&config).await?;
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name IN ('libraries', 'library_roots', 'scan_job_paths')
         ORDER BY name",
    )
    .fetch_all(database.pool())
    .await?;

    assert_eq!(tables, ["libraries", "library_roots", "scan_job_paths"]);
    Ok(())
}

#[tokio::test]
async fn new_libraries_enable_realtime_indexing_by_default()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database)
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;

    assert!(library.realtime_watch_enabled);
    assert_eq!(library.probe_concurrency, 64);
    Ok(())
}

#[tokio::test]
async fn library_probe_concurrency_accepts_128_without_raising_scan_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let service = LibraryService::new(database.clone());
    let library = service
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;

    let updated = service
        .update_settings(
            library.id,
            LibrarySettingsPatch {
                scan_concurrency: Some(64),
                probe_concurrency: Some(128),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(updated.library.scan_concurrency, 64);
    assert_eq!(updated.library.probe_concurrency, 128);

    let error = service
        .update_settings(
            library.id,
            LibrarySettingsPatch {
                scan_concurrency: Some(65),
                ..Default::default()
            },
        )
        .await
        .expect_err("scan concurrency must remain bounded separately");
    assert!(matches!(error, LibraryServiceError::InvalidConcurrency));
    Ok(())
}

#[tokio::test]
async fn new_libraries_register_safe_default_schedules() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let service = LibraryService::new(database.clone());

    let without_scraper = service
        .create_library("Local movies", LibraryKind::Movie, false)
        .await?;
    let with_scraper = service
        .create_library_with_scraper(
            "Online movies",
            LibraryKind::Movie,
            false,
            Some("tmdb"),
            false,
        )
        .await?;

    let schedules: Vec<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT task_type, cron_or_interval, is_enabled
         FROM scheduled_task_configs
         WHERE owner_type = 'LIBRARY' AND owner_id = ?
         ORDER BY task_type",
    )
    .bind(without_scraper.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        schedules,
        vec![
            ("METADATA_PARSE".to_owned(), None, 0),
            (
                "RECONCILIATION_SCAN".to_owned(),
                Some("0 3 * * 0".to_owned()),
                1
            ),
        ]
    );

    let online_metadata: (Option<String>, i64) = sqlx::query_as(
        "SELECT cron_or_interval, is_enabled
         FROM scheduled_task_configs
         WHERE owner_type = 'LIBRARY' AND owner_id = ? AND task_type = 'METADATA_PARSE'",
    )
    .bind(with_scraper.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(online_metadata, (Some("0 4 * * 0".to_owned()), 1));
    Ok(())
}

#[tokio::test]
async fn ordered_library_scrapers_persist_roles_and_legacy_primary_id()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let service = LibraryService::new(database.clone());
    let scrapers = vec![
        LibraryScraper {
            scraper_id: "tmdb".to_owned(),
            position: 0,
            role: LibraryScraperRole::Primary,
        },
        LibraryScraper {
            scraper_id: "douban".to_owned(),
            position: 1,
            role: LibraryScraperRole::Both,
        },
    ];

    let library = service
        .create_library_with_scrapers("Movies", LibraryKind::Movie, false, &scrapers, false)
        .await?;
    assert_eq!(library.scraper_id.as_deref(), Some("tmdb"));
    assert_eq!(library.scrapers, scrapers);

    let updated = service
        .update_settings(
            library.id,
            LibrarySettingsPatch {
                scrapers: Some(vec![LibraryScraper {
                    scraper_id: "imdb".to_owned(),
                    position: 0,
                    role: LibraryScraperRole::Primary,
                }]),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(updated.library.scraper_id.as_deref(), Some("imdb"));
    assert_eq!(updated.library.scrapers.len(), 1);
    assert_eq!(
        updated.library.scrapers[0].role,
        LibraryScraperRole::Primary
    );

    let stored_scraper: Option<String> = sqlx::query_scalar(
        "SELECT scraper_id FROM library_scrapers WHERE library_id = ? AND position = 0",
    )
    .bind(library.id.to_string())
    .fetch_optional(database.pool())
    .await?;
    assert_eq!(stored_scraper.as_deref(), Some("imdb"));
    Ok(())
}

#[tokio::test]
async fn chapter_sources_are_only_allowed_for_series_or_mixed_libraries()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let service = LibraryService::new(database.clone());

    let movie_error = service
        .create_library_with_scraper_and_chapter_source(
            "Movies",
            LibraryKind::Movie,
            false,
            None,
            Some("org.lux.intro-outro-detector"),
            false,
        )
        .await
        .expect_err("movie libraries cannot select a chapter source");
    assert!(matches!(
        movie_error,
        LibraryServiceError::InvalidChapterSourceId
    ));

    let mixed = service
        .create_library_with_scraper_and_chapter_source(
            "Mixed",
            LibraryKind::Mixed,
            false,
            None,
            Some("org.lux.intro-outro-detector"),
            false,
        )
        .await?;
    let updated = service
        .update_settings(
            mixed.id,
            luxd::application::libraries::LibrarySettingsPatch {
                kind: Some(LibraryKind::Movie),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(updated.library.kind, LibraryKind::Movie);
    assert_eq!(updated.library.chapter_source_id, None);

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn library_service_persists_multiple_roots_and_reports_overlap_rules()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let service = LibraryService::new(database);
    let movies_dir = temp_dir.path().join("Movies");
    let nested_dir = movies_dir.join("Nested");
    tokio::fs::create_dir_all(&nested_dir).await?;

    let library = service
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let first_root = service
        .add_root(library.id, movies_dir.to_str().ok_or("non-utf8 path")?)
        .await?;
    assert!(first_root.warnings.is_empty());
    assert!(first_root.root.is_available);

    let duplicate = service
        .add_root(library.id, movies_dir.to_str().ok_or("non-utf8 path")?)
        .await
        .expect_err("duplicate root");
    assert!(matches!(duplicate, LibraryServiceError::DuplicateRoot));

    let nested = service
        .add_root(library.id, nested_dir.to_str().ok_or("non-utf8 path")?)
        .await
        .expect_err("nested root");
    assert!(matches!(nested, LibraryServiceError::OverlappingRoot));

    let second_library = service
        .create_library("Archive", LibraryKind::Mixed, false)
        .await?;
    let cross_library = service
        .add_root(
            second_library.id,
            movies_dir.to_str().ok_or("non-utf8 path")?,
        )
        .await?;
    assert_eq!(
        cross_library.warnings,
        vec![LibraryWarningCode::CrossLibraryOverlap]
    );

    let views = service.list_libraries().await?;
    assert_eq!(views.len(), 2);
    assert_eq!(views[0].roots.len(), 1);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn read_only_root_is_saved_with_a_write_warning() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let service = LibraryService::new(database);
    let read_only_dir = temp_dir.path().join("ReadOnly");
    tokio::fs::create_dir(&read_only_dir).await?;
    let mut permissions = tokio::fs::metadata(&read_only_dir).await?.permissions();
    permissions.set_mode(0o555);
    tokio::fs::set_permissions(&read_only_dir, permissions).await?;

    let library = service
        .create_library("Read only", LibraryKind::Movie, false)
        .await?;
    let result = service
        .add_root(library.id, read_only_dir.to_str().ok_or("non-utf8 path")?)
        .await?;

    assert_eq!(result.warnings, vec![LibraryWarningCode::PathNotWritable]);
    assert!(!result.root.is_writable);
    Ok(())
}
