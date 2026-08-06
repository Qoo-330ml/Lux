use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use luxd::{
    application::libraries::{LibraryService, LibraryServiceError, LibraryWarningCode},
    config::Config,
    library::{LibraryKind, RootOverlap, classify_root_overlap, inspect_root_path},
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
