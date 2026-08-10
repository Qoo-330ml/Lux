use std::path::PathBuf;

use axum::extract::Path as AxumPath;
use axum::{Router, body::Body, http::StatusCode, response::Response, routing::get};
use luxd::{
    application::{
        images::{ImageDownloadConfig, ImageWriteError, ImageWriteService, write_image_atomically},
        libraries::LibraryService,
        scanner::LibraryScanner,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::CONTENT_TYPE;
use tokio::net::TcpListener;

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

#[tokio::test]
async fn downloads_missing_poster_and_fanart_and_refreshes_index()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, item_id, root, movie_dir) = prepared_movie().await?;
    let app = Router::new().route(
        "/{name}",
        get(|path: axum::extract::Path<String>| async move {
            let (content_type, body) = match path.0.as_str() {
                "poster" => ("image/png", PNG_1X1.to_vec()),
                "fanart" => ("image/webp", b"RIFF\x04\x00\x00\x00WEBP".to_vec()),
                _ => {
                    return Response::builder()
                        .status(StatusCode::NOT_FOUND)
                        .body(Body::empty())
                        .unwrap();
                }
            };
            Response::builder()
                .header(CONTENT_TYPE, content_type)
                .body(Body::from(body))
                .unwrap()
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let service = ImageWriteService::with_config(
        database.clone(),
        ImageDownloadConfig {
            max_bytes: 1024 * 1024,
            ..ImageDownloadConfig::default()
        },
    )?;
    let poster = service
        .download_item_image(&item_id, "poster", &format!("http://{address}/poster"))
        .await?;
    let fanart = service
        .download_item_image(&item_id, "fanart", &format!("http://{address}/fanart"))
        .await?;

    assert_eq!(poster.content_type, "image/png");
    let canonical_movie_dir = tokio::fs::canonicalize(&movie_dir).await?;
    assert_eq!(poster.path, canonical_movie_dir.join("poster.png"));
    assert_eq!(fanart.path, canonical_movie_dir.join("fanart.webp"));
    assert_eq!(tokio::fs::read(&poster.path).await?, PNG_1X1);
    assert_eq!(
        tokio::fs::read(&fanart.path).await?,
        b"RIFF\x04\x00\x00\x00WEBP"
    );

    let indexed: Vec<(String, String, i64, String)> = sqlx::query_as(
        "SELECT image_type, local_path, file_size, source FROM item_images WHERE item_id = ? ORDER BY image_type",
    )
    .bind(&item_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(indexed.len(), 2);
    assert_eq!(indexed[0].0, "FANART");
    assert_eq!(indexed[0].1, fanart.path.to_string_lossy());
    assert_eq!(indexed[0].2, 12);
    assert_eq!(indexed[0].3, "TMDB");
    assert_eq!(indexed[1].0, "POSTER");
    assert_eq!(indexed[1].1, poster.path.to_string_lossy());
    assert_eq!(indexed[1].2, PNG_1X1.len() as i64);
    assert_eq!(indexed[1].3, "TMDB");

    server.abort();
    let _ = root;
    Ok(())
}

#[tokio::test]
async fn downloads_flat_movie_images_to_distinct_media_prefixed_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, item_ids, _root, media_root) = prepared_flat_movies().await?;
    let app = Router::new().route(
        "/poster",
        get(|| async {
            Response::builder()
                .header(CONTENT_TYPE, "image/png")
                .body(Body::from(PNG_1X1.to_vec()))
                .expect("test image response should be valid")
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let service = ImageWriteService::new(database.clone())?;
    let first = service
        .download_item_image(&item_ids[0], "poster", &format!("http://{address}/poster"))
        .await?;
    let second = service
        .download_item_image(&item_ids[1], "poster", &format!("http://{address}/poster"))
        .await?;
    let canonical_media_root = tokio::fs::canonicalize(&media_root).await?;

    assert_eq!(
        first.path,
        canonical_media_root.join("Flat.One.2020-poster.png")
    );
    assert_eq!(
        second.path,
        canonical_media_root.join("Flat.Two.2021-poster.png")
    );
    assert_ne!(first.path, second.path);
    assert_eq!(tokio::fs::read(&first.path).await?, PNG_1X1);
    assert_eq!(tokio::fs::read(&second.path).await?, PNG_1X1);

    let skipped = service
        .download_item_image_if_missing(&item_ids[0], "poster", &format!("http://{address}/poster"))
        .await?;
    assert!(skipped.is_none());

    server.abort();
    Ok(())
}

#[tokio::test]
async fn episode_images_use_distinct_paths_and_ignore_season_artwork()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, episodes, season_dir) = prepared_episodes().await?;
    tokio::fs::write(season_dir.join("fanart.jpg"), b"season-artwork").await?;
    let mut second_image = PNG_1X1.to_vec();
    second_image.extend_from_slice(b"episode-two");
    let expected_second_image = second_image.clone();
    let app = Router::new().route(
        "/{name}",
        get(move |AxumPath(name): AxumPath<String>| {
            let body = if name == "episode-1" {
                PNG_1X1.to_vec()
            } else {
                second_image.clone()
            };
            async move {
                Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(body))
                    .expect("test image response should be valid")
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let service = ImageWriteService::new(database.clone())?;

    let first = service
        .download_item_image_if_missing(
            &episodes[0],
            "fanart",
            &format!("http://{address}/episode-1"),
        )
        .await?
        .ok_or("first episode image was incorrectly treated as present")?;
    let second = service
        .download_item_image_if_missing(
            &episodes[1],
            "fanart",
            &format!("http://{address}/episode-2"),
        )
        .await?
        .ok_or("second episode image was incorrectly treated as present")?;

    assert_ne!(first.path, second.path);
    assert!(
        first
            .path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains("Example.Show.S01E01-thumb"))
    );
    assert!(
        second
            .path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains("Example.Show.S01E02-thumb"))
    );
    assert_eq!(tokio::fs::read(&first.path).await?, PNG_1X1);
    assert_eq!(tokio::fs::read(&second.path).await?, expected_second_image);

    let indexed: Vec<(String, String)> = sqlx::query_as(
        "SELECT item_id, local_path FROM item_images WHERE image_type = 'FANART' ORDER BY item_id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(indexed.len(), 2);
    assert_ne!(indexed[0].1, indexed[1].1);
    assert!(indexed.iter().all(|(_, path)| path.contains("-thumb.png")));

    server.abort();
    Ok(())
}

#[tokio::test]
async fn rejects_bad_content_type_content_and_size_without_writing()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, item_id, _root, movie_dir) = prepared_movie().await?;
    let app = Router::new().route(
        "/{name}",
        get(|path: axum::extract::Path<String>| async move {
            match path.0.as_str() {
                "mime" => Response::builder()
                    .header(CONTENT_TYPE, "text/plain")
                    .body(Body::from(PNG_1X1.to_vec()))
                    .unwrap(),
                "content" => Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(b"not-an-image".to_vec()))
                    .unwrap(),
                "large" => Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(PNG_1X1.to_vec()))
                    .unwrap(),
                _ => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::empty())
                    .unwrap(),
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let service = ImageWriteService::with_config(database.clone(), ImageDownloadConfig::default())?;

    assert!(matches!(
        service
            .download_item_image(&item_id, "poster", &format!("http://{address}/mime"))
            .await,
        Err(ImageWriteError::UnsupportedContentType { .. })
    ));
    let service = ImageWriteService::with_config(
        database.clone(),
        ImageDownloadConfig {
            max_bytes: 1024,
            ..ImageDownloadConfig::default()
        },
    )?;
    assert!(matches!(
        service
            .download_item_image(&item_id, "poster", &format!("http://{address}/content"))
            .await,
        Err(ImageWriteError::InvalidContent { .. })
    ));
    let service = ImageWriteService::with_config(
        database.clone(),
        ImageDownloadConfig {
            max_bytes: 8,
            ..ImageDownloadConfig::default()
        },
    )?;
    assert!(matches!(
        service
            .download_item_image(&item_id, "poster", &format!("http://{address}/large"))
            .await,
        Err(ImageWriteError::TooLarge { .. })
    ));
    assert!(!movie_dir.join("poster.png").exists());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM item_images WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(count, 0);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn atomic_image_write_leaves_no_temp_file() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let target = directory.path().join("poster.png");
    write_image_atomically(&target, PNG_1X1).await?;
    assert_eq!(tokio::fs::read(&target).await?, PNG_1X1);
    let mut entries = tokio::fs::read_dir(directory.path()).await?;
    let entry = entries.next_entry().await?.ok_or("missing image")?;
    assert_eq!(entry.file_name(), "poster.png");
    assert!(entries.next_entry().await?.is_none());
    Ok(())
}

async fn prepared_movie() -> Result<(Database, String, PathBuf, PathBuf), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let root = temp_dir.keep().join("Movies");
    let movie_dir = root.join("Image Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Image.Movie.2020.mkv"), b"movie").await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: root.join("config"),
    };
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
    Ok((database, item_id, root, movie_dir))
}

async fn prepared_flat_movies()
-> Result<(Database, Vec<String>, PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let root = temp_dir.keep().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    for stem in ["Flat.One.2020", "Flat.Two.2021"] {
        tokio::fs::write(
            root.join(format!("{stem}.strm")),
            "https://example.invalid/flat",
        )
        .await?;
    }
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: root.join("config"),
    };
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
    let item_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'MOVIE' ORDER BY sort_title",
    )
    .fetch_all(database.pool())
    .await?;
    Ok((database, item_ids, root.clone(), root))
}

async fn prepared_episodes() -> Result<(Database, Vec<String>, PathBuf), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let root = temp_dir.keep().join("Shows");
    let season_dir = root.join("Example Show (2024)").join("Season 01");
    tokio::fs::create_dir_all(&season_dir).await?;
    for episode in [1, 2] {
        tokio::fs::write(
            season_dir.join(format!("Example.Show.S01E0{episode}.strm")),
            "https://example.invalid/episode",
        )
        .await?;
    }
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: root.join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    let episodes: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'EPISODE' ORDER BY episode_number",
    )
    .fetch_all(database.pool())
    .await?;
    Ok((database, episodes, season_dir))
}
