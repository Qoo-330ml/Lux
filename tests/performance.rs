use std::{
    collections::BTreeMap,
    env, fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use axum::{
    Router,
    body::Body,
    extract::{Path as AxumPath, State as AxumState},
    response::Response,
    routing::get,
};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        candidates::MetadataSelectionService,
        images::ImageWriteService,
        libraries::LibraryService,
        probe::{FfprobeRunner, MediaProbeService},
        reidentify::{MetadataRefreshMode, MetadataReidentifyService},
        scanner::LibraryScanner,
        scraper::{
            ScraperAdapter, ScraperCreditsResponse, ScraperError, ScraperExternalIdsResponse,
            ScraperFuture, ScraperGetRequest, ScraperImage, ScraperImageRequest,
            ScraperImagesResponse, ScraperMetadata, ScraperMetadataBundle, ScraperProvider,
            ScraperSearchRequest, ScraperSearchResponse, ScraperSearchResult,
            ScraperTrailersResponse,
        },
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    observability::resources::ResourceMetrics,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::json;
use tokio::net::TcpListener;

const FOREGROUND_REQUESTS: usize = 50;
const INCREMENTAL_FILES: usize = 100;
const METADATA_BENCHMARK_ITEMS: usize = 32;
const METADATA_BENCHMARK_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run with scripts/run-performance.sh for the LUX-045 ARM64 gate"]
async fn lux_045_catalog_scan_benchmark() -> Result<(), Box<dyn std::error::Error>> {
    let media_root = PathBuf::from(env::var("LUX_PERF_MEDIA_ROOT")?);
    let file_count: usize = env::var("LUX_PERF_FILE_COUNT")?.parse()?;
    assert!(file_count >= 60_000, "LUX-045 requires at least 60k files");
    assert!(media_root.join(".lux-fixture.json").is_file());

    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("Admin", "Admin", "performance-only password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Performance Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-utf8 fixture path")?,
        )
        .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(FOREGROUND_REQUESTS)
        .build()?;
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({
            "username": "admin",
            "password": "performance-only password"
        }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookies = format!(
        "lux_session={}",
        cookie_value(login.headers(), "lux_session")
    );

    let scanner = LibraryScanner::new(database.clone());
    let first_started = Instant::now();
    let first = scanner.scan_movie_library(library.id).await?;
    let first_ms = first_started.elapsed().as_millis();
    assert_eq!(first.discovered_files, file_count);
    assert_eq!(first.created_items, file_count);
    assert_eq!(first.created_sources, file_count);

    let unchanged_started = Instant::now();
    let scanner_for_unchanged = scanner.clone();
    let unchanged_handle = tokio::spawn(async move {
        scanner_for_unchanged
            .scan_movie_library(library.id)
            .await
            .map(|report| (report, unchanged_started.elapsed().as_millis()))
    });
    tokio::task::yield_now().await;
    let scan_running_before_api = !unchanged_handle.is_finished();
    let foreground_ms = measure_get_requests(
        &client,
        &format!("{base_url}/api/v1/admin/libraries"),
        &cookies,
        "foreground",
    )
    .await?;
    let catalog_list_ms = measure_get_requests(
        &client,
        &format!(
            "{base_url}/api/v1/libraries/{}/items?page=1&pageSize=50",
            library.id
        ),
        &cookies,
        "catalog list",
    )
    .await?;
    let catalog_search_single_started = Instant::now();
    let catalog_search_single_response = client
        .get(format!(
            "{base_url}/api/v1/search?q=Fixture&page=1&pageSize=50"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(
        catalog_search_single_response.status(),
        reqwest::StatusCode::OK
    );
    let catalog_search_single_ms = catalog_search_single_started.elapsed().as_millis();
    let catalog_search_ms = measure_get_requests(
        &client,
        &format!("{base_url}/api/v1/search?q=Fixture&page=1&pageSize=50"),
        &cookies,
        "catalog search",
    )
    .await?;
    let catalog_search_p95 = percentile(&catalog_search_ms, 95);
    assert!(
        catalog_search_p95 < 500,
        "catalog search p95 must stay below 500ms, got {catalog_search_p95}ms"
    );
    let (unchanged, unchanged_ms) = unchanged_handle.await??;
    assert_eq!(unchanged.discovered_files, file_count);
    assert_eq!(unchanged.created_items, 0);
    assert_eq!(unchanged.created_sources, 0);
    assert_eq!(unchanged.skipped_files, file_count);

    let incremental_directory = media_root.join("bucket-0000");
    for index in 60_000..60_000 + INCREMENTAL_FILES {
        let year = 2000 + index % 100;
        tokio::fs::write(
            incremental_directory.join(format!("Fixture.Movie.{index:06}.{year}.mkv")),
            b"LUX PERF INCREMENTAL FIXTURE\n",
        )
        .await?;
    }
    let incremental_started = Instant::now();
    let incremental = scanner
        .scan_movie_directory(library.id, &incremental_directory)
        .await?;
    let incremental_ms = incremental_started.elapsed().as_millis();
    assert_eq!(incremental.discovered_files, 100 + INCREMENTAL_FILES);
    assert_eq!(incremental.created_items, INCREMENTAL_FILES);
    assert_eq!(incremental.created_sources, INCREMENTAL_FILES);
    assert_eq!(incremental.skipped_files, 100);
    assert_eq!(incremental.marked_missing, 0);

    let non_pending_probe_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_sources WHERE probe_status <> 'PENDING'")
            .fetch_one(database.pool())
            .await?;
    let metadata_fingerprint_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items WHERE metadata_fingerprint IS NOT NULL",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(non_pending_probe_count, 0);
    assert_eq!(metadata_fingerprint_count, 0);

    println!(
        "LUX-045 RESULT {}",
        serde_json::to_string(&json!({
            "commit": luxd::COMMIT,
            "architecture": std::env::consts::ARCH,
            "fileCount": file_count,
            "firstScanMs": first_ms,
            "unchangedRescanMs": unchanged_ms,
            "incrementalDirectoryFiles": 100 + INCREMENTAL_FILES,
            "incrementalScanMs": incremental_ms,
            "foregroundRequestCount": FOREGROUND_REQUESTS,
            "foregroundDuringScan": scan_running_before_api,
            "foregroundP50Ms": percentile(&foreground_ms, 50),
            "foregroundP95Ms": percentile(&foreground_ms, 95),
            "catalogListP50Ms": percentile(&catalog_list_ms, 50),
            "catalogListP95Ms": percentile(&catalog_list_ms, 95),
            "catalogSearchSingleMs": catalog_search_single_ms,
            "catalogSearchP50Ms": percentile(&catalog_search_ms, 50),
            "catalogSearchP95Ms": catalog_search_p95,
            "foregroundErrors": 0,
            "nonPendingProbeCount": non_pending_probe_count,
            "metadataFingerprintCount": metadata_fingerprint_count,
            "targetForegroundP95Ms": 1000,
        }))?
    );

    server.abort();
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run with scripts/run-performance.sh for the LUX-197 ffprobe gate"]
async fn lux_197_ffprobe_concurrency_benchmark() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("FFprobe benchmark", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    let media_dir = media_root.join("Benchmark Movie (2024)");
    tokio::fs::create_dir_all(&media_dir).await?;
    for index in 0..512 {
        tokio::fs::write(
            media_dir.join(format!("Benchmark.Movie.{index:03}.2024.mkv")),
            b"LUX FFPROBE BENCHMARK FIXTURE\n",
        )
        .await?;
    }
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let fake_ffprobe = temp_dir.path().join("fake-ffprobe");
    let script = r##"#!/usr/bin/env python3
import fcntl
from pathlib import Path
import time

state_dir = Path(__file__).resolve().parent / "state"
state_dir.mkdir(parents=True, exist_ok=True)
lock_path = state_dir / "lock"
current_path = state_dir / "current"
maximum_path = state_dir / "maximum"

def update_current(delta):
    with lock_path.open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            current = int(current_path.read_text() or "0") + delta
        except (FileNotFoundError, ValueError):
            current = max(delta, 0)
        current_path.write_text(str(current))
        if delta > 0:
            try:
                maximum = int(maximum_path.read_text() or "0")
            except (FileNotFoundError, ValueError):
                maximum = 0
            if current > maximum:
                maximum_path.write_text(str(current))
        fcntl.flock(lock, fcntl.LOCK_UN)

update_current(1)
try:
    time.sleep(0.05)
finally:
    update_current(-1)
print('{"format":{"format_name":"matroska"},"streams":[]}', end="")
"##;
    fs::write(&fake_ffprobe, script)?;
    let mut permissions = fs::metadata(&fake_ffprobe)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake_ffprobe, permissions)?;

    let state_dir = fake_ffprobe
        .parent()
        .ok_or("missing benchmark parent")?
        .join("state");
    let mut results = Vec::new();
    for requested in [128_i64, 256, 384, 512] {
        let _ = fs::remove_dir_all(&state_dir);
        sqlx::query("UPDATE libraries SET probe_concurrency = ? WHERE id = ?")
            .bind(requested)
            .bind(library.id.to_string())
            .execute(database.pool())
            .await?;
        sqlx::query(
            "UPDATE media_sources
             SET probe_status = 'PENDING', probe_error = NULL, updated_at = unixepoch()",
        )
        .execute(database.pool())
        .await?;
        let started = Instant::now();
        let report = MediaProbeService::new(
            database.clone(),
            // High fan-out process launch can be slow on a loaded development
            // host; keep the fixture timeout above the production probe timeout
            // so it measures concurrency rather than host scheduling jitter.
            FfprobeRunner::new(&fake_ffprobe, std::time::Duration::from_secs(120)),
        )
        .probe_movie_library(library.id)
        .await?;
        let elapsed_ms = started.elapsed().as_millis();
        eprintln!("LUX-197 probe requested={requested} report={report:?}");
        let maximum = tokio::fs::read_to_string(state_dir.join("maximum"))
            .await?
            .trim()
            .parse::<usize>()?;
        assert_eq!(report.ready, 512);
        assert!(maximum > 0 && maximum <= requested as usize);
        results.push(serde_json::json!({
            "requested": requested,
            "observed": maximum,
            "elapsedMs": elapsed_ms,
        }));
    }
    println!(
        "LUX-197 FFPROBE RESULT {}",
        serde_json::to_string(&serde_json::json!({
            "architecture": std::env::consts::ARCH,
            "fileCount": 512,
            "levels": results,
        }))?
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run with scripts/run-metadata-performance.sh for the LUX-200 metadata gate"]
async fn lux_200_metadata_pipeline_benchmark() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("LUX-200 metadata benchmark", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("Movies");
    for index in 0..METADATA_BENCHMARK_ITEMS {
        let movie_dir = media_root.join(format!("Benchmark Movie {index:03} (2024)"));
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(
            movie_dir.join(format!("Benchmark.Movie.{index:03}.2024.mkv")),
            b"LUX-200 METADATA BENCHMARK FIXTURE\n",
        )
        .await?;
    }
    libraries
        .add_root(
            library.id,
            media_root
                .to_str()
                .ok_or("non-utf8 metadata fixture path")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let image_state = MetadataBenchmarkImageState::default();
    let image_server = Router::new()
        .route("/image/{name}", get(metadata_benchmark_image))
        .with_state(image_state);
    let image_listener = TcpListener::bind("127.0.0.1:0").await?;
    let image_address = image_listener.local_addr()?;
    let image_server_task = tokio::spawn(async move {
        axum::serve(image_listener, image_server)
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))
    });

    let scraper = ScraperProvider::from_adapter(MetadataBenchmarkScraper::new(format!(
        "http://{image_address}/image"
    )));
    let selection = MetadataSelectionService::with_config_dir(
        database.clone(),
        ImageWriteService::new_with_config_dir(database.clone(), config.config_dir.clone())?,
        config.config_dir.clone(),
    );
    let resources = ResourceMetrics::new();
    let metadata =
        MetadataReidentifyService::with_selection(database.clone(), scraper, Some(selection))
            .with_resource_metrics(resources.clone());
    let job = metadata
        .create_library_refresh_job(&library.id.to_string(), MetadataRefreshMode::FillMissing)
        .await?;
    let started = Instant::now();
    metadata.run(&job.id).await;
    let elapsed = started.elapsed();
    let completed = metadata.get_job(&job.id).await?;
    assert_eq!(
        completed.status, "COMPLETED",
        "metadata benchmark item results: {:?}",
        completed.items
    );
    assert_eq!(completed.total_count, METADATA_BENCHMARK_ITEMS as i64);

    let image_attempt_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT item_id) FROM metadata_image_attempts
         WHERE status IN ('AVAILABLE', 'UNAVAILABLE', 'FAILED')",
    )
    .fetch_one(database.pool())
    .await?;
    let image_available_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT item_id) FROM metadata_image_attempts
         WHERE status = 'AVAILABLE'",
    )
    .fetch_one(database.pool())
    .await?;
    let image_unavailable_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT item_id) FROM metadata_image_attempts
         WHERE status = 'UNAVAILABLE'",
    )
    .fetch_one(database.pool())
    .await?;
    let snapshot = resources.snapshot().await;
    let counters = snapshot.metadata.counters;
    let stage_p95_ms = snapshot.metadata.stage_p95_ms;
    assert_eq!(
        counters.get("request.search.count"),
        Some(&(METADATA_BENCHMARK_ITEMS as u64))
    );
    assert_eq!(
        counters.get("request.bundle.count"),
        Some(&(METADATA_BENCHMARK_ITEMS as u64))
    );
    assert!(counters.contains_key("stage.item_total.count"));
    assert!(stage_p95_ms.contains_key("item_total"));
    assert_eq!(image_available_items, (METADATA_BENCHMARK_ITEMS - 4) as i64);
    assert_eq!(image_unavailable_items, 4);
    assert_eq!(counters.get("retry.image_download.count"), Some(&1));
    assert_eq!(image_attempt_items, METADATA_BENCHMARK_ITEMS as i64);

    let image_unavailable_ratio = image_unavailable_items as f64 / image_attempt_items as f64;
    let image_retry_count = counters
        .get("retry.image_download.count")
        .copied()
        .unwrap_or_default();
    let image_retry_ratio = image_retry_count as f64 / image_attempt_items as f64;
    let elapsed_seconds = elapsed.as_secs_f64().max(0.001);
    println!(
        "LUX-200 METADATA RESULT {}",
        serde_json::to_string(&json!({
            "commit": luxd::COMMIT,
            "architecture": std::env::consts::ARCH,
            "itemCount": METADATA_BENCHMARK_ITEMS,
            "elapsedMs": elapsed.as_millis(),
            "itemsPerSecond": METADATA_BENCHMARK_ITEMS as f64 / elapsed_seconds,
            "requestCounters": counters,
            "stageP95Ms": stage_p95_ms,
            "scraperRetryCount": counters
                .iter()
                .filter(|(key, _)| key.starts_with("retry.") && !key.ends_with("image_download.count"))
                .map(|(_, value)| *value)
                .sum::<u64>(),
            "imageRetryCount": image_retry_count,
            "imageRetryRatio": image_retry_ratio,
            "imageAttemptItems": image_attempt_items,
            "imageAvailableItems": image_available_items,
            "imageUnavailableItems": image_unavailable_items,
            "imageUnavailableRatio": image_unavailable_ratio,
            "imageBytes": counters.get("image.bytes").copied().unwrap_or_default(),
        }))?
    );

    image_server_task.abort();
    Ok(())
}

#[derive(Clone)]
struct MetadataBenchmarkScraper {
    image_base_url: String,
    resources: Arc<Mutex<Option<ResourceMetrics>>>,
}

impl MetadataBenchmarkScraper {
    fn new(image_base_url: String) -> Self {
        Self {
            image_base_url,
            resources: Arc::new(Mutex::new(None)),
        }
    }

    fn record(&self, capability: &'static str, started: Instant) {
        let Ok(resources) = self.resources.lock() else {
            return;
        };
        if let Some(resources) = resources.as_ref() {
            resources.record_metadata_request(capability, false);
            resources.record_metadata_stage(capability, started.elapsed());
        }
    }

    fn provider_id(requested: &str) -> String {
        requested
            .parse::<u64>()
            .map(|value| value.to_string())
            .unwrap_or_else(|_| "10000".to_owned())
    }

    fn index(provider_id: &str) -> usize {
        provider_id
            .parse::<usize>()
            .ok()
            .and_then(|value| value.checked_sub(10_000))
            .unwrap_or_default()
    }

    fn metadata(provider_id: &str) -> ScraperMetadata {
        let index = Self::index(provider_id);
        ScraperMetadata {
            item_type: Some("Movie".to_owned()),
            title: Some(format!("Benchmark Movie {index:03}")),
            overview: Some(format!("Benchmark overview {index}")),
            production_year: Some(2024),
            premiere_date: Some("2024-01-01".to_owned()),
            original_language: Some("en".to_owned()),
            provider_ids: BTreeMap::from([("Benchmark".to_owned(), provider_id.to_owned())]),
            ..ScraperMetadata::default()
        }
    }

    fn images(&self, provider_id: &str) -> ScraperImagesResponse {
        if Self::index(provider_id) % 8 == 0 {
            return ScraperImagesResponse::default();
        }
        ScraperImagesResponse {
            images: vec![ScraperImage {
                image_type: "Primary".to_owned(),
                url: format!("{}/poster-{provider_id}", self.image_base_url),
                ..ScraperImage::default()
            }],
        }
    }
}

impl ScraperAdapter for MetadataBenchmarkScraper {
    fn provider_key(&self) -> &str {
        "benchmark"
    }

    fn search(
        &self,
        request: ScraperSearchRequest,
    ) -> ScraperFuture<'_, Result<ScraperSearchResponse, ScraperError>> {
        let scraper = self.clone();
        Box::pin(async move {
            let started = Instant::now();
            tokio::time::sleep(Duration::from_millis(2)).await;
            let index = request
                .name
                .rsplit(' ')
                .next()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_default();
            let provider_id = (10_000 + index).to_string();
            scraper.record("metadata.search", started);
            Ok(ScraperSearchResponse {
                items: vec![ScraperSearchResult {
                    item_type: Some("Movie".to_owned()),
                    title: Some(request.name),
                    overview: Some(format!("Benchmark overview {index}")),
                    production_year: request.year,
                    provider_ids: BTreeMap::from([("Benchmark".to_owned(), provider_id)]),
                    ..ScraperSearchResult::default()
                }],
            })
        })
    }

    fn get(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperMetadata, ScraperError>> {
        let scraper = self.clone();
        Box::pin(async move {
            let started = Instant::now();
            tokio::time::sleep(Duration::from_millis(2)).await;
            let provider_id = Self::provider_id(&request.provider_id);
            let metadata = Self::metadata(&provider_id);
            scraper.record("metadata.get", started);
            Ok(metadata)
        })
    }

    fn bundle(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperMetadataBundle, ScraperError>> {
        let scraper = self.clone();
        Box::pin(async move {
            let started = Instant::now();
            tokio::time::sleep(Duration::from_millis(2)).await;
            let provider_id = Self::provider_id(&request.provider_id);
            let images = scraper.images(&provider_id);
            let result = ScraperMetadataBundle {
                metadata: Self::metadata(&provider_id),
                images,
                credits: ScraperCreditsResponse::default(),
                external_ids: ScraperExternalIdsResponse {
                    provider_ids: BTreeMap::from([("Imdb".to_owned(), format!("tt{provider_id}"))]),
                },
                trailers: ScraperTrailersResponse::default(),
            };
            scraper.record("metadata.bundle", started);
            Ok(result)
        })
    }

    fn images(
        &self,
        request: ScraperImageRequest,
    ) -> ScraperFuture<'_, Result<ScraperImagesResponse, ScraperError>> {
        let scraper = self.clone();
        Box::pin(async move {
            let started = Instant::now();
            tokio::time::sleep(Duration::from_millis(2)).await;
            let provider_id = Self::provider_id(&request.provider_id);
            let result = scraper.images(&provider_id);
            scraper.record("metadata.images", started);
            Ok(result)
        })
    }

    fn credits(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperCreditsResponse, ScraperError>> {
        let scraper = self.clone();
        Box::pin(async move {
            let started = Instant::now();
            tokio::time::sleep(Duration::from_millis(2)).await;
            let _ = request;
            scraper.record("metadata.credits", started);
            Ok(ScraperCreditsResponse::default())
        })
    }

    fn external_ids(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperExternalIdsResponse, ScraperError>> {
        let scraper = self.clone();
        Box::pin(async move {
            let started = Instant::now();
            tokio::time::sleep(Duration::from_millis(2)).await;
            let provider_id = Self::provider_id(&request.provider_id);
            scraper.record("metadata.externalIds", started);
            Ok(ScraperExternalIdsResponse {
                provider_ids: BTreeMap::from([("Imdb".to_owned(), format!("tt{provider_id}"))]),
            })
        })
    }

    fn trailers(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperTrailersResponse, ScraperError>> {
        let scraper = self.clone();
        Box::pin(async move {
            let started = Instant::now();
            tokio::time::sleep(Duration::from_millis(2)).await;
            let _ = request;
            scraper.record("metadata.trailers", started);
            Ok(ScraperTrailersResponse::default())
        })
    }

    fn with_resource_metrics(&self, resources: ResourceMetrics) {
        if let Ok(mut current) = self.resources.lock() {
            *current = Some(resources);
        }
    }
}

#[derive(Clone, Default)]
struct MetadataBenchmarkImageState {
    retries: Arc<Mutex<BTreeMap<String, usize>>>,
}

async fn metadata_benchmark_image(
    AxumState(state): AxumState<MetadataBenchmarkImageState>,
    AxumPath(name): AxumPath<String>,
) -> Response {
    if name == "poster-10001" {
        let Ok(mut retries) = state.retries.lock() else {
            return Response::builder()
                .status(500)
                .body(Body::empty())
                .unwrap_or_else(|_| Response::new(Body::empty()));
        };
        let attempts = retries.entry(name.clone()).or_default();
        if *attempts == 0 {
            *attempts += 1;
            return Response::builder()
                .status(503)
                .body(Body::empty())
                .unwrap_or_else(|_| Response::new(Body::empty()));
        }
    }
    Response::builder()
        .header("content-type", "image/png")
        .body(Body::from(METADATA_BENCHMARK_PNG.to_vec()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

async fn measure_get_requests(
    client: &reqwest::Client,
    url: &str,
    cookies: &str,
    label: &str,
) -> Result<Vec<u128>, Box<dyn std::error::Error>> {
    let mut requests = Vec::with_capacity(FOREGROUND_REQUESTS);
    for _ in 0..FOREGROUND_REQUESTS {
        let client = client.clone();
        let url = url.to_owned();
        let cookies = cookies.to_owned();
        let label = label.to_owned();
        requests.push(tokio::spawn(async move {
            let started = Instant::now();
            let response = client
                .get(url)
                .header(COOKIE, cookies)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status();
            let _ = response.bytes().await.map_err(|error| error.to_string())?;
            if status != reqwest::StatusCode::OK {
                return Err(format!("{label} request returned {status}"));
            }
            Ok::<u128, String>(started.elapsed().as_millis())
        }));
    }
    let mut durations = Vec::with_capacity(FOREGROUND_REQUESTS);
    for request in requests {
        let result = request
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        durations.push(result.map_err(std::io::Error::other)?);
    }
    Ok(durations)
}

fn percentile(values: &[u128], percentile: usize) -> u128 {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() * percentile).saturating_add(99) / 100).saturating_sub(1);
    sorted[index.min(sorted.len().saturating_sub(1))]
}

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';')?;
            let (cookie_name, cookie_value) = pair.split_once('=')?;
            (cookie_name == name).then(|| cookie_value.to_owned())
        })
        .expect("expected cookie")
}
