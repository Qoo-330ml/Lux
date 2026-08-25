use std::{env, fs, path::PathBuf, time::Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        probe::{FfprobeRunner, MediaProbeService},
        scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::json;
use tokio::net::TcpListener;

const FOREGROUND_REQUESTS: usize = 50;
const INCREMENTAL_FILES: usize = 100;

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
            "catalogSearchP95Ms": percentile(&catalog_search_ms, 95),
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
    let script = concat!(
        "#!/bin/sh\n",
        "state_dir=\"$(dirname \"$0\")/state\"\n",
        "mkdir -p \"$state_dir\"\n",
        "while ! mkdir \"$state_dir/lock\" 2>/dev/null; do sleep 0.001; done\n",
        "current=$(cat \"$state_dir/current\" 2>/dev/null || printf '0')\n",
        "current=$((current + 1))\n",
        "printf '%s' \"$current\" > \"$state_dir/current\"\n",
        "maximum=$(cat \"$state_dir/maximum\" 2>/dev/null || printf '0')\n",
        "if [ \"$current\" -gt \"$maximum\" ]; then printf '%s' \"$current\" > \"$state_dir/maximum\"; fi\n",
        "rmdir \"$state_dir/lock\"\n",
        "sleep 0.05\n",
        "while ! mkdir \"$state_dir/lock\" 2>/dev/null; do sleep 0.001; done\n",
        "current=$(cat \"$state_dir/current\")\n",
        "printf '%s' \"$((current - 1))\" > \"$state_dir/current\"\n",
        "rmdir \"$state_dir/lock\"\n",
        "printf '%s' '{\"format\":{\"format_name\":\"matroska\"},\"streams\":[]}'\n",
    );
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
