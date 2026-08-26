use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::extract::Path as AxumPath;
use axum::{Router, body::Body, http::StatusCode, response::Response, routing::get};
use luxd::{
    application::{
        images::{ImageDownloadConfig, ImageWriteError, ImageWriteService, write_image_atomically},
        libraries::LibraryService,
        metadata_paths::library_item_directory,
        scanner::LibraryScanner,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::CONTENT_TYPE;
use tokio::net::TcpListener;
use tokio::time::{Duration, sleep};

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
async fn image_downloads_respect_the_global_concurrency_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, item_id, _root, _movie_dir) = prepared_movie().await?;
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let handler_active = Arc::clone(&active);
    let handler_maximum = Arc::clone(&maximum);
    let app = Router::new().route(
        "/{name}",
        get(move || {
            let active = Arc::clone(&handler_active);
            let maximum = Arc::clone(&handler_maximum);
            async move {
                let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(current, Ordering::SeqCst);
                sleep(Duration::from_millis(40)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(PNG_1X1.to_vec()))
                    .expect("test image response should be valid")
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let service = ImageWriteService::with_config_and_concurrency(
        database,
        ImageDownloadConfig::default(),
        2,
    )?;

    let mut tasks = Vec::new();
    for image_type in ["POSTER", "FANART", "LOGO", "BANNER", "DISC", "ART"] {
        let service = service.clone();
        let item_id = item_id.clone();
        let url = format!("http://{address}/{image_type}");
        tasks.push(tokio::spawn(async move {
            service
                .download_item_image(&item_id, image_type, &url)
                .await
        }));
    }
    for task in tasks {
        task.await??;
    }

    assert!(maximum.load(Ordering::SeqCst) <= 2);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn concurrent_downloads_claim_one_attempt_for_the_same_image()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, item_id, _root, _movie_dir) = prepared_movie().await?;
    let requests = Arc::new(AtomicUsize::new(0));
    let handler_requests = Arc::clone(&requests);
    let app = Router::new().route(
        "/poster",
        get(move || {
            let requests = Arc::clone(&handler_requests);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                sleep(Duration::from_millis(40)).await;
                Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(PNG_1X1.to_vec()))
                    .expect("test image response should be valid")
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let service = ImageWriteService::with_config(database, ImageDownloadConfig::default())?;
    let image_url = format!("http://{address}/poster");
    let first = service.clone();
    let second = service.clone();
    let first_item_id = item_id.clone();
    let second_item_id = item_id;
    let first_url = image_url.clone();
    let second_url = image_url;
    let (first, second) = tokio::join!(
        tokio::spawn(async move {
            first
                .download_item_image_if_missing(&first_item_id, "poster", &first_url)
                .await
        }),
        tokio::spawn(async move {
            second
                .download_item_image_if_missing(&second_item_id, "poster", &second_url)
                .await
        }),
    );

    let first = first??;
    let second = second??;
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(first.is_some() ^ second.is_some());

    server.abort();
    Ok(())
}

#[tokio::test]
async fn manual_scraper_image_selection_claims_one_attempt_for_the_same_image()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, item_id, _root, _movie_dir) = prepared_movie().await?;
    let requests = Arc::new(AtomicUsize::new(0));
    let handler_requests = Arc::clone(&requests);
    let app = Router::new().route(
        "/poster",
        get(move || {
            let requests = Arc::clone(&handler_requests);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                sleep(Duration::from_millis(40)).await;
                Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(PNG_1X1.to_vec()))
                    .expect("test image response should be valid")
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let service = ImageWriteService::new(database)?;
    let image_url = format!("http://{address}/poster");
    let first = service.clone();
    let second = service;
    let first_item_id = item_id.clone();
    let second_item_id = item_id;
    let first_url = image_url.clone();
    let second_url = image_url;
    let (first, second) = tokio::join!(
        tokio::spawn(async move {
            first
                .download_item_image_from_scraper_candidate(&first_item_id, "poster", &first_url)
                .await
        }),
        tokio::spawn(async move {
            second
                .download_item_image_from_scraper_candidate(&second_item_id, "poster", &second_url)
                .await
        }),
    );

    let first = first?;
    let second = second?;
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert!(first.is_ok() ^ second.is_ok());
    assert!(
        first
            .as_ref()
            .err()
            .or_else(|| second.as_ref().err())
            .is_some_and(|error| matches!(error, ImageWriteError::AttemptInProgress))
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn missing_image_not_found_is_not_requested_again_on_next_attempt()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, item_id, _root, _movie_dir) = prepared_movie().await?;
    let requests = Arc::new(AtomicUsize::new(0));
    let handler_requests = Arc::clone(&requests);
    let app = Router::new().route(
        "/missing",
        get(move || {
            let requests = Arc::clone(&handler_requests);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Body::empty())
                    .expect("not-found response should be valid")
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let service = ImageWriteService::with_config(database, ImageDownloadConfig::default())?;
    let image_url = format!("http://{address}/missing");

    assert!(
        service
            .download_item_image_if_missing(&item_id, "poster", &image_url)
            .await
            .is_err()
    );
    assert!(
        service
            .download_item_image_if_missing(&item_id, "poster", &image_url)
            .await?
            .is_none()
    );

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn transient_image_failure_is_skipped_until_retry_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, item_id, _root, _movie_dir) = prepared_movie().await?;
    let requests = Arc::new(AtomicUsize::new(0));
    let handler_requests = Arc::clone(&requests);
    let app = Router::new().route(
        "/temporary",
        get(move || {
            let requests = Arc::clone(&handler_requests);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                Response::builder()
                    .status(StatusCode::SERVICE_UNAVAILABLE)
                    .body(Body::empty())
                    .expect("temporary failure response should be valid")
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let service = ImageWriteService::with_config(database.clone(), ImageDownloadConfig::default())?;
    let image_url = format!("http://{address}/temporary");

    assert!(
        service
            .download_item_image_if_missing(&item_id, "poster", &image_url)
            .await
            .is_err()
    );
    let after_first = requests.load(Ordering::SeqCst);
    assert!(after_first > 0);
    let attempt: (String, Option<i64>) = sqlx::query_as(
        "SELECT status, next_retry_at
         FROM metadata_image_attempts
         WHERE item_id = ?",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(attempt.0, "FAILED");
    assert!(attempt.1.is_some());

    assert!(
        service
            .download_item_image_if_missing(&item_id, "poster", &image_url)
            .await?
            .is_none()
    );
    assert_eq!(requests.load(Ordering::SeqCst), after_first);

    sqlx::query(
        "UPDATE metadata_image_attempts
         SET next_retry_at = 0
         WHERE item_id = ?",
    )
    .bind(&item_id)
    .execute(database.pool())
    .await?;
    assert!(
        service
            .download_item_image_if_missing(&item_id, "poster", &image_url)
            .await
            .is_err()
    );
    assert!(requests.load(Ordering::SeqCst) > after_first);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn permanent_image_failure_is_not_retried_automatically()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, item_id, _root, _movie_dir) = prepared_movie().await?;
    let requests = Arc::new(AtomicUsize::new(0));
    let handler_requests = Arc::clone(&requests);
    let app = Router::new().route(
        "/invalid",
        get(move || {
            let requests = Arc::clone(&handler_requests);
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                Response::builder()
                    .header(CONTENT_TYPE, "image/png")
                    .body(Body::from(b"broken".to_vec()))
                    .expect("invalid image response should be valid")
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let service = ImageWriteService::with_config(database, ImageDownloadConfig::default())?;
    let image_url = format!("http://{address}/invalid");

    assert!(
        service
            .download_item_image_if_missing(&item_id, "poster", &image_url)
            .await
            .is_err()
    );
    assert!(
        service
            .download_item_image_if_missing(&item_id, "poster", &image_url)
            .await?
            .is_none()
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn config_managed_images_use_the_metadata_library_directory()
-> Result<(), Box<dyn std::error::Error>> {
    let (database, item_id, root, _movie_dir) = prepared_movie().await?;
    let config_dir = root.join("config");
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

    let service = ImageWriteService::new_with_config_dir(database.clone(), config_dir.clone())?;
    let report = service
        .download_item_image(&item_id, "poster", &format!("http://{address}/poster"))
        .await?;
    let expected = tokio::fs::canonicalize(library_item_directory(&config_dir, &item_id)?)
        .await?
        .join("poster.png");
    assert_eq!(report.path, expected);
    let canonical_metadata_root = tokio::fs::canonicalize(config_dir.join("metadata")).await?;
    assert!(report.path.starts_with(canonical_metadata_root));
    assert_eq!(tokio::fs::read(&report.path).await?, PNG_1X1);

    let indexed: String =
        sqlx::query_scalar("SELECT local_path FROM item_images WHERE item_id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(indexed, report.path.to_string_lossy());
    assert!(
        service
            .download_item_image_if_missing(&item_id, "poster", &format!("http://{address}/poster"))
            .await?
            .is_none()
    );
    server.abort();
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn config_managed_image_writes_reject_metadata_parent_symlinks()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let (database, item_id, root, _movie_dir) = prepared_movie().await?;
    let config_dir = root.join("config");
    tokio::fs::create_dir_all(&config_dir).await?;
    let external_metadata = root.join("external-metadata");
    tokio::fs::create_dir_all(&external_metadata).await?;
    symlink(&external_metadata, config_dir.join("metadata"))?;

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

    let service = ImageWriteService::new_with_config_dir(database, config_dir)?;
    let error = service
        .download_item_image(&item_id, "poster", &format!("http://{address}/poster"))
        .await
        .expect_err("metadata symlink should be rejected");
    assert!(matches!(error, ImageWriteError::SymlinkTarget(_)));
    assert!(!external_metadata.join("library").exists());
    server.abort();
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
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
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
