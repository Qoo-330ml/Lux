use luxd::{
    application::{
        libraries::LibraryService,
        metadata::{
            ImageType, LocalImage, MetadataEnricher, NfoMetadata, find_local_images, parse_nfo,
        },
        scanner::LibraryScanner,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[test]
fn nfo_parser_reads_local_fields_and_ignores_unknown_fields() {
    let metadata = parse_nfo(
        r#"<movie><title>本地标题</title><originaltitle>Original</originaltitle><year>2021</year><plot>简介</plot><unknown>忽略</unknown></movie>"#.as_bytes(),
    )
    .expect("valid nfo");

    assert_eq!(
        metadata,
        NfoMetadata {
            title: Some("本地标题".to_owned()),
            original_title: Some("Original".to_owned()),
            production_year: Some(2021),
            overview: Some("简介".to_owned()),
        }
    );
}

#[test]
fn malformed_nfo_is_rejected() {
    assert!(parse_nfo(b"<movie><title>broken").is_err());
}

#[test]
fn partial_and_empty_nfo_are_accepted() {
    let partial = parse_nfo(b"<movie><title>Only Title</title></movie>").expect("partial nfo");
    assert_eq!(partial.title.as_deref(), Some("Only Title"));
    assert_eq!(partial.production_year, None);
    assert_eq!(
        parse_nfo(b"<movie/>").expect("empty nfo"),
        NfoMetadata::default()
    );
}

#[test]
fn image_discovery_only_returns_supported_poster_and_fanart_files() {
    let paths = [
        "/media/poster.jpg",
        "/media/fanart.png",
        "/media/poster.txt",
        "/media/thumb.jpg",
    ];

    let images = find_local_images(paths.iter().map(std::path::Path::new));

    assert_eq!(
        images,
        vec![
            LocalImage {
                image_type: ImageType::Poster,
                path: std::path::PathBuf::from("/media/poster.jpg"),
            },
            LocalImage {
                image_type: ImageType::Fanart,
                path: std::path::PathBuf::from("/media/fanart.png"),
            },
        ]
    );
}

#[tokio::test]
async fn metadata_enrichment_updates_items_and_keeps_bad_nfo_non_blocking()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let good_dir = root.join("Good Movie (2020)");
    let bad_dir = root.join("Broken Movie (2021)");
    tokio::fs::create_dir_all(&good_dir).await?;
    tokio::fs::create_dir_all(&bad_dir).await?;
    tokio::fs::write(good_dir.join("Good.Movie.2020.mkv"), b"movie").await?;
    tokio::fs::write(
        good_dir.join("movie.nfo"),
        r#"<movie><title>本地电影</title><originaltitle>Local Movie</originaltitle><year>2021</year><plot>本地简介</plot></movie>"#,
    )
    .await?;
    tokio::fs::write(good_dir.join("poster.jpg"), b"poster").await?;
    tokio::fs::write(good_dir.join("fanart.png"), b"fanart").await?;
    tokio::fs::write(bad_dir.join("Broken.Movie.2021.mkv"), b"movie").await?;
    tokio::fs::write(bad_dir.join("Broken.Movie.2021.nfo"), b"<movie><title>").await?;

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

    let report = MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;
    assert_eq!(report.nfo_loaded, 1);
    assert_eq!(report.nfo_failed, 1);
    assert_eq!(report.images_found, 2);

    let item: (String, String, i64, String) = sqlx::query_as(
        "SELECT title, original_title, production_year, overview
         FROM media_items WHERE sort_title = 'good movie'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        item,
        (
            "本地电影".to_owned(),
            "Local Movie".to_owned(),
            2021,
            "本地简介".to_owned()
        )
    );
    let image_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_images")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(image_count, 2);
    let image_metadata: (i64, String) =
        sqlx::query_as("SELECT file_size, source FROM item_images ORDER BY image_type LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(image_metadata, (6, "LOCAL".to_owned()));

    let second_report = MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;
    assert_eq!(second_report.nfo_loaded, 0);
    assert_eq!(second_report.nfo_failed, 0);
    assert_eq!(second_report.nfo_skipped, 2);
    assert_eq!(second_report.images_found, 0);
    let image_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_images")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(image_count, 2);
    Ok(())
}
