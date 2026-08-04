use luxd::{
    application::{
        libraries::LibraryService,
        metadata::NfoMetadata,
        nfo::{NfoWriteService, rewrite_nfo, write_nfo_atomically},
        scanner::LibraryScanner,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[test]
fn nfo_rewrite_updates_common_fields_and_preserves_unknown_xml()
-> Result<(), Box<dyn std::error::Error>> {
    let original = r#"<?xml version="1.0" encoding="UTF-8"?>
<movie><title>旧标题</title><year>2020</year><custom><keep>保留</keep></custom></movie>"#;
    let rewritten = rewrite_nfo(
        original.as_bytes(),
        &NfoMetadata {
            title: Some("新标题".to_owned()),
            overview: Some("简介".to_owned()),
            production_year: Some(2024),
            ..NfoMetadata::default()
        },
    )?;
    let text = String::from_utf8(rewritten)?;
    assert!(text.contains("<title>新标题</title>"));
    assert!(text.contains("<year>2024</year>"));
    assert!(text.contains("<plot>简介</plot>"));
    assert!(text.contains("<custom><keep>保留</keep></custom>"));
    assert!(!text.contains("旧标题"));
    Ok(())
}

#[test]
fn nfo_rewrite_creates_a_movie_document_when_target_is_missing() {
    let rewritten = rewrite_nfo(
        &[],
        &NfoMetadata {
            title: Some("新电影".to_owned()),
            ..NfoMetadata::default()
        },
    )
    .expect("new movie nfo");
    let text = String::from_utf8(rewritten).expect("utf8 nfo");
    assert!(text.starts_with("<movie>"));
    assert!(text.contains("<title>新电影</title>"));
}

#[tokio::test]
async fn atomic_writer_replaces_target_without_leaving_a_temp_file()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let target = temp_dir.path().join("movie.nfo");
    tokio::fs::write(&target, b"<movie><title>old</title></movie>").await?;

    write_nfo_atomically(
        &target,
        &NfoMetadata {
            title: Some("new".to_owned()),
            ..NfoMetadata::default()
        },
    )
    .await?;

    assert!(String::from_utf8(tokio::fs::read(&target).await?)?.contains("<title>new</title>"));
    let mut entries = tokio::fs::read_dir(temp_dir.path()).await?;
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        names.push(entry.file_name());
    }
    assert_eq!(names, vec![std::ffi::OsString::from("movie.nfo")]);
    Ok(())
}

#[tokio::test]
async fn nfo_service_checks_library_root_and_refreshes_metadata_fingerprint()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"fixture").await?;
    tokio::fs::write(
        movie_dir.join("movie.nfo"),
        "<movie><custom>keep</custom><title>old</title></movie>",
    )
    .await?;
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
        .fetch_one(database.pool())
        .await?;

    let report = NfoWriteService::new(database.clone())
        .write_item_nfo(
            &item_id,
            &NfoMetadata {
                title: Some("new".to_owned()),
                overview: Some("overview".to_owned()),
                ..NfoMetadata::default()
            },
        )
        .await?;
    let output = tokio::fs::read_to_string(&report.path).await?;
    assert!(output.contains("<custom>keep</custom>"));
    assert!(output.contains("<title>new</title>"));
    let fingerprint: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT metadata_fingerprint FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(fingerprint, Some(report.fingerprint));
    Ok(())
}

#[tokio::test]
async fn nfo_service_writes_next_to_strm_source() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Example STRM Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(
        movie_dir.join("Example.STRM.Movie.2020.strm"),
        "https://example.invalid/movie",
    )
    .await?;
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
        .fetch_one(database.pool())
        .await?;

    let report = NfoWriteService::new(database)
        .write_item_nfo(
            &item_id,
            &NfoMetadata {
                title: Some("已识别 STRM 电影".to_owned()),
                ..NfoMetadata::default()
            },
        )
        .await?;

    let canonical_movie_dir = tokio::fs::canonicalize(&movie_dir).await?;
    assert_eq!(report.path, canonical_movie_dir.join("movie.nfo"));
    let output = tokio::fs::read_to_string(&report.path).await?;
    assert!(output.contains("<title>已识别 STRM 电影</title>"));
    Ok(())
}

#[tokio::test]
async fn malformed_original_is_not_replaced() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let target = temp_dir.path().join("movie.nfo");
    let original = b"<movie><title>broken";
    tokio::fs::write(&target, original).await?;

    let result = write_nfo_atomically(
        &target,
        &NfoMetadata {
            title: Some("new".to_owned()),
            ..NfoMetadata::default()
        },
    )
    .await;
    assert!(result.is_err());
    assert_eq!(tokio::fs::read(&target).await?, original);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn read_only_directory_rejects_nfo_write() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir()?;
    let directory = temp_dir.path().join("ReadOnly");
    tokio::fs::create_dir(&directory).await?;
    let mut permissions = tokio::fs::metadata(&directory).await?.permissions();
    permissions.set_mode(0o555);
    tokio::fs::set_permissions(&directory, permissions).await?;
    let result = write_nfo_atomically(
        &directory.join("movie.nfo"),
        &NfoMetadata {
            title: Some("new".to_owned()),
            ..NfoMetadata::default()
        },
    )
    .await;
    let mut restore = tokio::fs::metadata(&directory).await?.permissions();
    restore.set_mode(0o755);
    tokio::fs::set_permissions(&directory, restore).await?;
    assert!(matches!(
        result,
        Err(luxd::application::nfo::NfoWriteError::Io { .. })
    ));
    Ok(())
}
