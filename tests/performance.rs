use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{BufRead, BufReader, Read, Write},
    net::SocketAddr,
    path::PathBuf,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, Path as AxumPath, State as AxumState},
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
        scanner::{LibraryScanner, ScanJobService},
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
    config::{Config, DatabaseConfiguration, PostgresConnection},
    library::LibraryKind,
    observability::resources::ResourceMetrics,
    storage::Database,
};
use reqwest::header::{COOKIE, HeaderMap, HeaderValue, SET_COOKIE};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpListener;
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{
    layer::{Context, Layer},
    prelude::*,
};

const FOREGROUND_REQUESTS: usize = 50;
const INCREMENTAL_FILES: usize = 100;
const METADATA_BENCHMARK_ITEMS: usize = 32;
const POOL_PRESSURE_SAMPLE_INTERVAL: Duration = Duration::from_millis(5);
const POSTER_API_PROBE_INTERVAL: Duration = Duration::from_millis(100);
const POSTER_API_PROBE_MAX_REQUESTS: usize = 20;
const POSTER_API_PROBE_MAX_DURATION: Duration = Duration::from_secs(5);
const POSTER_API_PROBE_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const POSTER_CATALOG_PAGE_SIZE: i64 = 100;
static NEXT_PERFORMANCE_PROBE_REQUEST_ID: AtomicUsize = AtomicUsize::new(1);
static ISOLATED_LOADGEN_CHILDREN: OnceLock<Mutex<BTreeMap<String, PersistentLoadgenChild>>> =
    OnceLock::new();

struct PersistentLoadgenChild {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl PersistentLoadgenChild {
    fn spawn(executable: PathBuf) -> std::io::Result<Self> {
        let mut child = Command::new(executable)
            .args([
                "--exact",
                "isolated_performance_loadgen_child",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("loadgen child did not expose its input pipe"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("loadgen child did not expose its output pipe"))?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        })
    }

    fn request(&mut self, input: &str) -> std::io::Result<String> {
        self.stdin.write_all(input.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;

        let mut line = String::new();
        loop {
            line.clear();
            if self.stdout.read_line(&mut line)? == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "isolated loadgen child closed its output before returning a result",
                ));
            }
            if let Some((_, result)) = line.split_once("LUX_PERF_LOADGEN_RESULT:") {
                return Ok(result.trim().to_owned());
            }
        }
    }
}

struct IsolatedLoadgenChildGuard {
    origin: String,
}

impl IsolatedLoadgenChildGuard {
    fn new(url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            origin: isolated_loadgen_origin(url)?,
        })
    }
}

impl Drop for IsolatedLoadgenChildGuard {
    fn drop(&mut self) {
        let Some(children) = ISOLATED_LOADGEN_CHILDREN.get() else {
            return;
        };
        let Ok(mut children) = children.lock() else {
            return;
        };
        let Some(mut child) = children.remove(&self.origin) else {
            return;
        };
        drop(child.stdin);
        let mut trailing_output = String::new();
        let _ = child.stdout.read_to_string(&mut trailing_output);
        let _ = child.child.wait();
    }
}

fn isolated_loadgen_origin(url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let parsed_url = reqwest::Url::parse(url)?;
    if parsed_url.scheme() != "http"
        || !matches!(
            parsed_url.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
        )
    {
        return Err("isolated loadgen only accepts local HTTP test servers".into());
    }
    Ok(parsed_url.origin().ascii_serialization())
}

fn catalog_item_with_poster(
    body: &serde_json::Value,
    expected_item_id: &str,
) -> Option<(String, String)> {
    body.get("items")?.as_array()?.iter().find_map(|item| {
        let item_id = item.get("id")?.as_str()?;
        if item_id != expected_item_id {
            return None;
        }
        let poster_tag = item.get("imageTags")?.get("poster")?.as_str()?;
        if item_id.trim().is_empty() || poster_tag.trim().is_empty() {
            return None;
        }
        Some((item_id.to_owned(), poster_tag.to_owned()))
    })
}

fn catalog_page_contains_item(body: &serde_json::Value, expected_item_id: &str) -> bool {
    body.get("items")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("id").and_then(serde_json::Value::as_str) == Some(expected_item_id)
            })
        })
}

fn catalog_poster_page_for_movie_position(position: i64) -> Option<i64> {
    (position > 0).then_some((position - 1) / POSTER_CATALOG_PAGE_SIZE + 1)
}

fn scan_job_state_is_active(status: &str, phase: &str) -> bool {
    matches!(status, "PENDING" | "QUEUED" | "RUNNING")
        || (status == "COMPLETED" && phase == "POSTPROCESSING")
}

#[test]
fn scan_activity_includes_pending_jobs_and_excludes_terminal_idle_jobs() {
    assert!(scan_job_state_is_active("PENDING", "IDLE"));
    assert!(scan_job_state_is_active("QUEUED", "IDLE"));
    assert!(scan_job_state_is_active("RUNNING", "IDLE"));
    assert!(scan_job_state_is_active("COMPLETED", "POSTPROCESSING"));
    assert!(!scan_job_state_is_active("COMPLETED", "IDLE"));
    assert!(!scan_job_state_is_active("FAILED", "POSTPROCESSING"));
}

#[test]
fn catalog_poster_probe_selects_an_item_with_a_poster_tag() {
    image::load_from_memory(METADATA_BENCHMARK_PNG)
        .expect("benchmark poster fixture must decode as an image");
    let body = json!({
        "items": [
            {"id": "no-poster", "imageTags": {"poster": null}},
            {"id": "", "imageTags": {"poster": "ignored-empty-item"}},
            {"id": "movie-1", "imageTags": {"poster": "poster-tag-1"}},
            {"id": "movie-2", "imageTags": {"poster": "poster-tag-2"}}
        ]
    });
    assert_eq!(
        catalog_item_with_poster(&body, "movie-1"),
        Some(("movie-1".to_owned(), "poster-tag-1".to_owned()))
    );
    assert!(catalog_item_with_poster(&json!({"items": []}), "movie-1").is_none());
}

#[test]
fn catalog_poster_probe_uses_the_page_containing_the_ranked_movie() {
    assert_eq!(catalog_poster_page_for_movie_position(1), Some(1));
    assert_eq!(catalog_poster_page_for_movie_position(100), Some(1));
    assert_eq!(catalog_poster_page_for_movie_position(101), Some(2));
    assert_eq!(catalog_poster_page_for_movie_position(10_000), Some(100));
    assert_eq!(catalog_poster_page_for_movie_position(0), None);
}

#[test]
fn catalog_poster_probe_selects_the_indexed_item_from_its_page() {
    let body = json!({
        "items": [
            {"id": "other-movie", "imageTags": {"poster": "other-poster"}},
            {"id": "indexed-movie", "imageTags": {"poster": "indexed-poster"}}
        ]
    });
    assert_eq!(
        catalog_item_with_poster(&body, "indexed-movie"),
        Some(("indexed-movie".to_owned(), "indexed-poster".to_owned()))
    );
    assert!(catalog_page_contains_item(&body, "indexed-movie"));
    assert!(!catalog_page_contains_item(&body, "not-on-this-page"));
    assert!(catalog_item_with_poster(&body, "not-on-this-page").is_none());
}

#[derive(Debug, Default)]
struct CatalogPosterApiProbe {
    local_poster_index_observed: bool,
    catalog_page: Option<i64>,
    catalog_item_visible_ms: Option<u128>,
    catalog_rank_lookup_count: usize,
    catalog_list_request_count: usize,
    poster_endpoint_request_count: usize,
    item_id: Option<String>,
    poster_tag: Option<String>,
    poster_tag_visible_ms: Option<u128>,
    poster_visible_ms: Option<u128>,
    poster_endpoint_latency_ms: Option<u128>,
    content_type: Option<String>,
    body_bytes: Option<usize>,
    body_decoded_as_image: bool,
    last_error: Option<String>,
}

#[test]
fn process_peak_rss_is_reported_in_bytes() {
    assert!(process_peak_rss_bytes().is_some_and(|bytes| bytes > 0));
}

fn process_peak_rss_bytes() -> Option<u64> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage initializes the provided structure on success.
        let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        if result != 0 {
            return None;
        }
        // SAFETY: result == 0 proves getrusage initialized usage.
        let usage = unsafe { usage.assume_init() };
        let native_value = u64::try_from(usage.ru_maxrss).ok()?;
        #[cfg(target_os = "macos")]
        let bytes = native_value;
        #[cfg(target_os = "linux")]
        let bytes = native_value.checked_mul(1024)?;
        Some(bytes)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

const METADATA_BENCHMARK_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00,
    0x00, 0x05, 0x00, 0x01, 0x7a, 0x5e, 0xab, 0x3f, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];
type ScanStageSample = (u64, u64, u64, u64);

#[derive(Default)]
struct QueryLatencySamples {
    call_counts: std::collections::HashMap<String, usize>,
    durations_us: std::collections::HashMap<String, Vec<u64>>,
}

#[derive(Default)]
struct QueryStatementCounts {
    statements: AtomicUsize,
    dml_statements: AtomicUsize,
    dml_summaries: Mutex<std::collections::HashMap<String, usize>>,
    query_latency_samples: Mutex<QueryLatencySamples>,
    unclassified_cte_summaries: Mutex<std::collections::HashMap<String, usize>>,
    manifest_application_ms: AtomicUsize,
    manifest_transaction_ms: AtomicUsize,
    manifest_apply_timing_batches: AtomicUsize,
    manifest_positive_commit_batches: AtomicUsize,
    manifest_active_job_selects: AtomicUsize,
    manifest_preparation_concurrency: AtomicUsize,
    manifest_directory_read_concurrency: AtomicUsize,
    active_preparation_tasks_peak: AtomicUsize,
    active_directory_readers_peak: AtomicUsize,
    scan_stage_samples: Mutex<std::collections::HashMap<String, Vec<ScanStageSample>>>,
}

impl QueryStatementCounts {
    fn reset(&self) {
        self.statements.store(0, Ordering::Relaxed);
        self.dml_statements.store(0, Ordering::Relaxed);
        self.manifest_application_ms.store(0, Ordering::Relaxed);
        self.manifest_transaction_ms.store(0, Ordering::Relaxed);
        self.manifest_apply_timing_batches
            .store(0, Ordering::Relaxed);
        self.manifest_positive_commit_batches
            .store(0, Ordering::Relaxed);
        self.manifest_active_job_selects.store(0, Ordering::Relaxed);
        self.manifest_preparation_concurrency
            .store(0, Ordering::Relaxed);
        self.manifest_directory_read_concurrency
            .store(0, Ordering::Relaxed);
        self.active_preparation_tasks_peak
            .store(0, Ordering::Relaxed);
        self.active_directory_readers_peak
            .store(0, Ordering::Relaxed);
        self.dml_summaries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        *self
            .query_latency_samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = QueryLatencySamples::default();
        self.unclassified_cte_summaries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.scan_stage_samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    fn snapshot(&self) -> (usize, usize) {
        (
            self.statements.load(Ordering::Relaxed),
            self.dml_statements.load(Ordering::Relaxed),
        )
    }

    fn manifest_apply_timing_snapshot(&self) -> (usize, usize, usize) {
        (
            self.manifest_application_ms.load(Ordering::Relaxed),
            self.manifest_transaction_ms.load(Ordering::Relaxed),
            self.manifest_apply_timing_batches.load(Ordering::Relaxed),
        )
    }

    fn manifest_positive_commit_batch_count(&self) -> usize {
        self.manifest_positive_commit_batches
            .load(Ordering::Relaxed)
    }

    fn manifest_active_job_select_count(&self) -> usize {
        self.manifest_active_job_selects.load(Ordering::Relaxed)
    }

    fn manifest_directory_concurrency_snapshot(&self) -> (usize, usize) {
        (
            self.manifest_preparation_concurrency
                .load(Ordering::Relaxed),
            self.manifest_directory_read_concurrency
                .load(Ordering::Relaxed),
        )
    }

    fn scan_stage_snapshot(&self) -> Vec<ScanStageSummary> {
        let samples = self
            .scan_stage_samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut summaries = samples
            .iter()
            .filter_map(|(phase, samples)| scan_stage_summary(phase, samples))
            .collect::<Vec<_>>();
        summaries.sort_unstable_by(|left, right| left.phase.cmp(&right.phase));
        summaries
    }

    fn scan_stage_peaks(&self) -> (usize, usize) {
        (
            self.active_preparation_tasks_peak.load(Ordering::Relaxed),
            self.active_directory_readers_peak.load(Ordering::Relaxed),
        )
    }

    fn scan_stage_values(&self) -> serde_json::Value {
        let (preparation_peak, reader_peak) = self.scan_stage_peaks();
        serde_json::json!({
            "stages": self.scan_stage_snapshot().into_iter().map(|summary| serde_json::json!({
                "phase": summary.phase,
                "calls": summary.call_count,
                "cumulativeDurationUs": summary.total_duration_us,
                "p50DurationUs": summary.p50_duration_us,
                "p95DurationUs": summary.p95_duration_us,
                "units": summary.total_units,
                "files": summary.total_files,
                "directories": summary.total_directories,
            })).collect::<Vec<_>>(),
            "activePreparationTasksPeak": preparation_peak,
            "activeDirectoryReadersPeak": reader_peak,
            "durationNote": "Cumulative stage durations are work totals, not wall time; nested and concurrent stages may overlap."
        })
    }

    fn take_query_latency_values(
        &self,
        phase_window: &str,
        background_sql_may_be_included: bool,
        phase_window_note: &str,
    ) -> serde_json::Value {
        let samples = std::mem::take(
            &mut *self
                .query_latency_samples
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        query_latency_report(
            &samples.durations_us,
            &samples.call_counts,
            phase_window,
            background_sql_may_be_included,
            phase_window_note,
        )
    }

    fn dml_summary_snapshot(&self) -> Vec<(usize, String)> {
        let summaries = self
            .dml_summaries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut summaries = summaries
            .iter()
            .map(|(summary, count)| (*count, summary.clone()))
            .collect::<Vec<_>>();
        summaries.sort_unstable_by_key(|summary| std::cmp::Reverse(summary.0));
        summaries.truncate(20);
        summaries
    }

    fn dml_statement_count_containing(&self, marker: &str) -> usize {
        self.dml_summaries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .filter(|(summary, _)| summary.contains(marker))
            .map(|(_, count)| *count)
            .sum()
    }

    fn unclassified_cte_summary_snapshot(&self) -> Vec<(usize, String)> {
        let summaries = self
            .unclassified_cte_summaries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut summaries = summaries
            .iter()
            .map(|(summary, count)| (*count, summary.clone()))
            .collect::<Vec<_>>();
        summaries.sort_unstable_by_key(|summary| std::cmp::Reverse(summary.0));
        summaries.truncate(20);
        summaries
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ScanStageSummary {
    phase: String,
    call_count: usize,
    total_duration_us: u128,
    p50_duration_us: u64,
    p95_duration_us: u64,
    total_units: u128,
    total_files: u128,
    total_directories: u128,
}

fn scan_stage_summary(phase: &str, samples: &[ScanStageSample]) -> Option<ScanStageSummary> {
    if samples.is_empty() {
        return None;
    }
    let mut durations = samples.iter().map(|sample| sample.0).collect::<Vec<_>>();
    durations.sort_unstable();
    let percentile_index = |percentile: usize| {
        ((durations.len() * percentile).saturating_add(99) / 100)
            .saturating_sub(1)
            .min(durations.len().saturating_sub(1))
    };
    Some(ScanStageSummary {
        phase: phase.to_owned(),
        call_count: samples.len(),
        total_duration_us: durations.iter().map(|duration| u128::from(*duration)).sum(),
        p50_duration_us: durations[percentile_index(50)],
        p95_duration_us: durations[percentile_index(95)],
        total_units: samples.iter().map(|sample| u128::from(sample.1)).sum(),
        total_files: samples.iter().map(|sample| u128::from(sample.2)).sum(),
        total_directories: samples.iter().map(|sample| u128::from(sample.3)).sum(),
    })
}

#[derive(Default)]
struct QuerySummaryVisitor {
    summary: Option<String>,
    elapsed_secs: Option<f64>,
    application_ms: Option<u64>,
    transaction_ms: Option<u64>,
    phase: Option<String>,
    duration_us: Option<u64>,
    units: Option<u64>,
    files: Option<u64>,
    directories: Option<u64>,
    preparation_concurrency: Option<u64>,
    directory_read_concurrency: Option<u64>,
    active_preparation_tasks: Option<u64>,
    active_directory_readers: Option<u64>,
}

impl Visit for QuerySummaryVisitor {
    fn record_f64(&mut self, field: &Field, value: f64) {
        if field.name() == "elapsed_secs" {
            self.elapsed_secs = Some(value);
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "application_ms" => self.application_ms = Some(value),
            "transaction_ms" => self.transaction_ms = Some(value),
            "preparation_concurrency" => self.preparation_concurrency = Some(value),
            "directory_read_concurrency" => self.directory_read_concurrency = Some(value),
            "duration_us" => self.duration_us = Some(value),
            "units" => self.units = Some(value),
            "files" => self.files = Some(value),
            "directories" => self.directories = Some(value),
            "active_preparation_tasks" => self.active_preparation_tasks = Some(value),
            "active_directory_readers" => self.active_directory_readers = Some(value),
            _ => {}
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "summary" {
            self.summary = Some(value.to_owned());
        } else if field.name() == "phase" {
            self.phase = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "summary" {
            self.summary = Some(format!("{value:?}").trim_matches('"').to_owned());
        }
    }
}

struct QueryStatementLayer(Arc<QueryStatementCounts>);

fn is_dml_statement_summary(summary: &str) -> bool {
    let normalized = summary
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase();
    if ["INSERT ", "UPDATE ", "DELETE ", "REPLACE ", "TRUNCATE "]
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
    {
        return true;
    }
    let incoming_observation_cte = normalized
        .starts_with("WITH INCOMING (RELATIVE_PATH, OBSERVATION_SEQUENCE")
        || normalized.starts_with("WITH INCOMING ( RELATIVE_PATH, OBSERVATION_SEQUENCE");
    let truncated_path_observation_cte = normalized.starts_with("WITH INCOMING (RELATIVE_PATH, …")
        || normalized.starts_with("WITH INCOMING ( RELATIVE_PATH, …");
    let truncated_seen_paths_cte =
        normalized.starts_with("WITH INCOMING_SEEN_PATHS(RELATIVE_PATH) AS (VALUES …");
    let materialized_target_cte = normalized.starts_with("WITH PAGE_SOURCES AS MATERIALIZED");
    if incoming_observation_cte
        || truncated_path_observation_cte
        || truncated_seen_paths_cte
        || materialized_target_cte
    {
        return true;
    }
    normalized.starts_with("WITH ")
        && [
            " INSERT INTO ",
            " UPDATE ",
            " DELETE FROM ",
            " REPLACE INTO ",
            " TRUNCATE ",
        ]
        .iter()
        .any(|keyword| normalized.contains(keyword))
}

fn is_manifest_active_job_select(summary: &str) -> bool {
    summary
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
        .starts_with("SELECT MANIFEST.LIBRARY_ID, JOB.GENERATION ")
}

#[test]
fn query_counter_classifies_dml_statements_inside_common_table_expressions() {
    assert!(is_dml_statement_summary(
        "WITH sidecar_directories(directory) AS (VALUES (?)) INSERT INTO scan_job_targets"
    ));
    assert!(is_dml_statement_summary(
        "WITH page_sources AS MATERIALIZED (SELECT id FROM filesystem_entries) INSERT INTO scan_job_targets"
    ));
    assert!(is_dml_statement_summary(
        "WITH page_sources AS MATERIALIZED (SELECT id FROM filesystem_entries)"
    ));
    assert!(is_dml_statement_summary(
        "WITH page_sources AS MATERIALIZED"
    ));
    assert!(!is_dml_statement_summary(
        "WITH latest AS (SELECT path FROM scan_manifest_entries) SELECT path FROM latest"
    ));
    assert!(is_dml_statement_summary(
        "WITH incoming (relative_path, observation_sequence) AS (VALUES (?, ?)) INSERT INTO scan_manifest_entries SELECT * FROM incoming"
    ));
    assert!(is_dml_statement_summary(
        "WITH incoming ( relative_path, observation_sequence, entry_kind, size, modified_at, device, inode, fingerprint)"
    ));
    assert!(!is_dml_statement_summary(
        "WITH incoming (relative_path) AS (VALUES (?)) SELECT path FROM scan_manifest_deltas"
    ));
    assert!(!is_dml_statement_summary(
        "WITH incoming (relative_path) AS (VALUES (?)) SELECT relative_path FROM scan_manifest_entries"
    ));
    assert!(is_dml_statement_summary(
        "WITH incoming_seen_paths(relative_path) AS (VALUES …"
    ));
}

#[test]
fn query_counter_recognizes_the_duplicate_manifest_active_job_select() {
    assert!(is_manifest_active_job_select(
        "SELECT manifest.library_id, job.generation FROM scan_manifests manifest"
    ));
    assert!(!is_manifest_active_job_select(
        "SELECT manifest.workflow_version, manifest.discovery_format_version FROM scan_manifests manifest"
    ));
}

#[test]
fn scan_stage_summary_reports_overlapping_microsecond_work_without_calling_it_wall_time() {
    let summary = scan_stage_summary("directory_read", &[(1_000, 8, 8, 2), (2_000, 4, 4, 1)]);
    assert_eq!(
        summary,
        Some(ScanStageSummary {
            phase: "directory_read".to_owned(),
            call_count: 2,
            total_duration_us: 3_000,
            p50_duration_us: 1_000,
            p95_duration_us: 2_000,
            total_units: 12,
            total_files: 12,
            total_directories: 3,
        })
    );
}

#[test]
fn scan_stage_summary_omits_empty_stages() {
    assert_eq!(scan_stage_summary("known_paths", &[]), None);
}

#[test]
fn query_latency_summaries_report_percentiles_and_sort_by_total_time() {
    let samples = std::collections::HashMap::from([
        ("SELECT FAST".to_owned(), vec![10, 20, 30]),
        ("SELECT SLOW".to_owned(), vec![100, 200, 300]),
    ]);
    let calls = std::collections::HashMap::from([
        ("SELECT FAST".to_owned(), 3),
        ("SELECT SLOW".to_owned(), 3),
    ]);

    let summaries = summarize_query_latencies(&samples, &calls);

    assert_eq!(summaries[0].summary, "SELECT SLOW");
    assert_eq!(summaries[0].call_count, 3);
    assert_eq!(summaries[0].cumulative_duration_us, 600);
    assert_eq!(summaries[0].duration_sample_count, 3);
    assert_eq!(summaries[0].p50_duration_us, Some(200));
    assert_eq!(summaries[0].p95_duration_us, Some(300));
    assert_eq!(summaries[0].max_duration_us, Some(300));
    assert_eq!(summaries[1].summary, "SELECT FAST");
    assert_eq!(summaries[1].cumulative_duration_us, 60);
}

#[test]
fn query_latency_report_marks_missing_elapsed_samples_unavailable() {
    let calls = std::collections::HashMap::from([("SELECT 1".to_owned(), 2)]);
    let report = query_latency_report(
        &std::collections::HashMap::new(),
        &calls,
        "test_phase",
        false,
        "test fixture only",
    );

    assert_eq!(report["phaseWindow"], "test_phase");
    assert_eq!(report["backgroundSqlMayBeIncluded"], false);
    assert_eq!(report["phaseWindowNote"], "test fixture only");
    assert_eq!(report["status"], "unavailable");
    assert_eq!(report["sampleCount"], 0);
    assert_eq!(
        report["unavailableReason"],
        "SQLx emitted no elapsed_secs events for this phase"
    );
    assert_eq!(report["callCount"], 2);
    assert_eq!(report["statements"][0]["callCount"], 2);
    assert_eq!(report["statements"][0]["durationStatus"], "unavailable");
    assert!(report["statements"][0]["p95DurationUs"].is_null());
}

#[test]
fn query_latency_summaries_normalize_whitespace_and_case() {
    let samples = std::collections::HashMap::from([
        ("select  id   from media_items".to_owned(), vec![10]),
        ("SELECT id from media_items".to_owned(), vec![20]),
    ]);

    let calls = std::collections::HashMap::from([
        ("select  id   from media_items".to_owned(), 1),
        ("SELECT id from media_items".to_owned(), 1),
    ]);
    let summaries = summarize_query_latencies(&samples, &calls);

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].summary, "SELECT ID FROM MEDIA_ITEMS");
    assert_eq!(summaries[0].call_count, 2);
    assert_eq!(summaries[0].cumulative_duration_us, 30);
}

#[test]
fn pool_pressure_report_marks_empty_sample_windows_unavailable() {
    let report = pool_pressure_measurement_report(0, 8, 0, 0, 0, 0);

    assert!(report["metrics"].is_object());
    assert_eq!(report["metrics"]["status"], "unavailable");
    assert_eq!(report["metrics"]["sampleCount"], 0);
    assert_eq!(
        report["metrics"]["unavailableReason"],
        "no pool snapshots were captured during this measurement window"
    );
    assert!(report["metrics"]["maxSize"].is_null());
    assert!(report["metrics"]["maxIdle"].is_null());
    assert!(report["metrics"]["maxInUse"].is_null());
    assert!(report["metrics"]["saturatedSamples"].is_null());
    assert!(report["metrics"]["observedSaturated"].is_null());
}

#[test]
fn local_poster_queue_requires_all_batches_and_image_markers_to_complete() {
    let complete = LocalPosterQueueSnapshot {
        batch_count: 3,
        completed_batch_count: 3,
        completed_with_image_marker_count: 3,
        pending_or_running_batch_count: 0,
        failed_batch_count: 0,
        cancelled_batch_count: 0,
        missing_image_marker_count: 0,
        poster_item_count: 100,
    };
    assert!(complete.is_drained(100));
    assert!(!complete.is_pending_measurement_window());

    let pending = LocalPosterQueueSnapshot {
        batch_count: 3,
        completed_batch_count: 2,
        completed_with_image_marker_count: 2,
        pending_or_running_batch_count: 1,
        missing_image_marker_count: 1,
        ..complete.clone()
    };
    assert!(pending.is_pending_measurement_window());

    for incomplete in [
        LocalPosterQueueSnapshot {
            completed_batch_count: 2,
            completed_with_image_marker_count: 2,
            pending_or_running_batch_count: 1,
            missing_image_marker_count: 1,
            ..complete.clone()
        },
        LocalPosterQueueSnapshot {
            completed_with_image_marker_count: 2,
            missing_image_marker_count: 1,
            ..complete.clone()
        },
        LocalPosterQueueSnapshot {
            failed_batch_count: 1,
            ..complete.clone()
        },
        LocalPosterQueueSnapshot {
            cancelled_batch_count: 1,
            ..complete.clone()
        },
        LocalPosterQueueSnapshot {
            poster_item_count: 99,
            ..complete.clone()
        },
        LocalPosterQueueSnapshot {
            batch_count: 0,
            completed_batch_count: 0,
            completed_with_image_marker_count: 0,
            ..complete.clone()
        },
    ] {
        assert!(!incomplete.is_drained(100), "{incomplete:?}");
    }
    let failed_with_pending = LocalPosterQueueSnapshot {
        failed_batch_count: 1,
        ..pending.clone()
    };
    assert!(!failed_with_pending.is_pending_measurement_window());
    assert!(
        failed_with_pending
            .pending_measurement_unavailable_reason()
            .contains("failed")
    );

    let cancelled_with_pending = LocalPosterQueueSnapshot {
        cancelled_batch_count: 1,
        ..pending.clone()
    };
    assert!(!cancelled_with_pending.is_pending_measurement_window());
    assert!(
        cancelled_with_pending
            .pending_measurement_unavailable_reason()
            .contains("cancelled")
    );

    let completed_missing_marker = LocalPosterQueueSnapshot {
        completed_batch_count: 3,
        completed_with_image_marker_count: 2,
        pending_or_running_batch_count: 0,
        missing_image_marker_count: 1,
        ..complete
    };
    assert!(completed_missing_marker.has_completed_batch_missing_image_marker());
    assert!(!completed_missing_marker.is_pending_measurement_window());
    assert!(
        completed_missing_marker
            .pending_measurement_unavailable_reason()
            .contains("images_completed_at")
    );
}

fn stage_names(value: &serde_json::Value) -> std::collections::HashSet<&str> {
    value["stages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|stage| stage["phase"].as_str())
        .collect()
}

impl<S> Layer<S> for QueryStatementLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        if event.metadata().target() == "lux::scan_performance" {
            let mut visitor = QuerySummaryVisitor::default();
            event.record(&mut visitor);
            if visitor.phase.as_deref() == Some("directory_read_budget") {
                if let Some(concurrency) = visitor.preparation_concurrency {
                    self.0.manifest_preparation_concurrency.fetch_max(
                        usize::try_from(concurrency).unwrap_or(usize::MAX),
                        Ordering::Relaxed,
                    );
                }
                if let Some(concurrency) = visitor.directory_read_concurrency {
                    self.0.manifest_directory_read_concurrency.fetch_max(
                        usize::try_from(concurrency).unwrap_or(usize::MAX),
                        Ordering::Relaxed,
                    );
                }
                return;
            }
            if let Some(phase) = visitor.phase.as_deref()
                && let Some(duration_us) = visitor.duration_us
            {
                self.0
                    .scan_stage_samples
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .entry(phase.to_owned())
                    .or_default()
                    .push((
                        duration_us,
                        visitor.units.unwrap_or_default(),
                        visitor.files.unwrap_or_default(),
                        visitor.directories.unwrap_or_default(),
                    ));
            }
            if let Some(active_tasks) = visitor.active_preparation_tasks {
                self.0.active_preparation_tasks_peak.fetch_max(
                    usize::try_from(active_tasks).unwrap_or(usize::MAX),
                    Ordering::Relaxed,
                );
            }
            if let Some(active_readers) = visitor.active_directory_readers {
                self.0.active_directory_readers_peak.fetch_max(
                    usize::try_from(active_readers).unwrap_or(usize::MAX),
                    Ordering::Relaxed,
                );
            }
            if visitor.phase.as_deref() == Some("positive_commit") {
                self.0
                    .manifest_positive_commit_batches
                    .fetch_add(1, Ordering::Relaxed);
            }
            if visitor.application_ms.is_some() || visitor.transaction_ms.is_some() {
                self.0.manifest_application_ms.fetch_add(
                    usize::try_from(visitor.application_ms.unwrap_or_default())
                        .unwrap_or(usize::MAX),
                    Ordering::Relaxed,
                );
                self.0.manifest_transaction_ms.fetch_add(
                    usize::try_from(visitor.transaction_ms.unwrap_or_default())
                        .unwrap_or(usize::MAX),
                    Ordering::Relaxed,
                );
                self.0
                    .manifest_apply_timing_batches
                    .fetch_add(1, Ordering::Relaxed);
            }
            return;
        }
        if event.metadata().target() != "sqlx::query" {
            return;
        }
        self.0.statements.fetch_add(1, Ordering::Relaxed);
        let mut visitor = QuerySummaryVisitor::default();
        event.record(&mut visitor);
        let summary =
            normalize_sqlx_statement_summary(visitor.summary.as_deref().unwrap_or_default());
        let is_postgres_lock_monitor = summary.starts_with("SELECT COUNT(*) FROM PG_STAT_ACTIVITY");
        if !summary.is_empty() && !is_postgres_lock_monitor {
            let elapsed_us = visitor
                .elapsed_secs
                .filter(|elapsed_secs| elapsed_secs.is_finite() && *elapsed_secs >= 0.0)
                .map(|elapsed_secs| {
                    (elapsed_secs * 1_000_000.0)
                        .round()
                        .clamp(0.0, u64::MAX as f64) as u64
                });
            let mut samples = self
                .0
                .query_latency_samples
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *samples.call_counts.entry(summary.clone()).or_default() += 1;
            if let Some(elapsed_us) = elapsed_us {
                samples
                    .durations_us
                    .entry(summary.clone())
                    .or_default()
                    .push(elapsed_us);
            }
        }
        if is_manifest_active_job_select(&summary) {
            self.0
                .manifest_active_job_selects
                .fetch_add(1, Ordering::Relaxed);
        }
        if is_dml_statement_summary(&summary) {
            self.0.dml_statements.fetch_add(1, Ordering::Relaxed);
            *self
                .0
                .dml_summaries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(summary)
                .or_default() += 1;
        } else if summary.starts_with("WITH ") {
            *self
                .0
                .unclassified_cte_summaries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(summary)
                .or_default() += 1;
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct QueryLatencySummary {
    summary: String,
    call_count: usize,
    duration_sample_count: usize,
    duration_status: &'static str,
    cumulative_duration_us: u128,
    p50_duration_us: Option<u64>,
    p95_duration_us: Option<u64>,
    max_duration_us: Option<u64>,
}

fn summarize_query_latencies(
    samples_by_summary: &std::collections::HashMap<String, Vec<u64>>,
    calls_by_summary: &std::collections::HashMap<String, usize>,
) -> Vec<QueryLatencySummary> {
    let mut normalized_samples = BTreeMap::<String, (usize, Vec<u64>)>::new();
    for (summary, call_count) in calls_by_summary {
        normalized_samples
            .entry(normalize_sqlx_statement_summary(summary))
            .or_default()
            .0 += *call_count;
    }
    for (summary, samples) in samples_by_summary {
        if !samples.is_empty() {
            normalized_samples
                .entry(normalize_sqlx_statement_summary(summary))
                .or_default()
                .1
                .extend_from_slice(samples);
        }
    }
    let mut summaries = normalized_samples
        .into_iter()
        .map(|(summary, (call_count, mut durations))| {
            durations.sort_unstable();
            let percentile_index = |percentile: usize| {
                ((durations.len() * percentile).saturating_add(99) / 100)
                    .saturating_sub(1)
                    .min(durations.len().saturating_sub(1))
            };
            QueryLatencySummary {
                summary,
                call_count: call_count.max(durations.len()),
                duration_sample_count: durations.len(),
                duration_status: if durations.is_empty() {
                    "unavailable"
                } else {
                    "available"
                },
                cumulative_duration_us: durations
                    .iter()
                    .map(|duration| u128::from(*duration))
                    .sum(),
                p50_duration_us: (!durations.is_empty()).then(|| durations[percentile_index(50)]),
                p95_duration_us: (!durations.is_empty()).then(|| durations[percentile_index(95)]),
                max_duration_us: durations.last().copied(),
            }
        })
        .collect::<Vec<_>>();
    summaries.sort_unstable_by(|left, right| {
        right
            .cumulative_duration_us
            .cmp(&left.cumulative_duration_us)
            .then_with(|| left.summary.cmp(&right.summary))
    });
    summaries
}

fn normalize_sqlx_statement_summary(summary: &str) -> String {
    summary
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}

fn query_latency_report(
    samples: &std::collections::HashMap<String, Vec<u64>>,
    calls: &std::collections::HashMap<String, usize>,
    phase_window: &str,
    background_sql_may_be_included: bool,
    phase_window_note: &str,
) -> serde_json::Value {
    let sample_count = samples.values().map(Vec::len).sum::<usize>();
    let call_count = calls.values().sum::<usize>();
    serde_json::json!({
        "phaseWindow": phase_window,
        "backgroundSqlMayBeIncluded": background_sql_may_be_included,
        "phaseWindowNote": phase_window_note,
        "status": if sample_count == 0 { "unavailable" } else { "available" },
        "callCount": call_count,
        "sampleCount": sample_count,
        "timingSampleCoverage":
            (call_count > 0).then(|| sample_count as f64 / call_count as f64),
        "unavailableReason": (sample_count == 0)
            .then_some("SQLx emitted no elapsed_secs events for this phase"),
        "durationUnit": "microseconds",
        "summarySource": "SQLx query summary (first four SQL tokens; bind values are not captured)",
        "durationNote": "SQLx elapsed_secs measures statement execution after acquiring a pooled connection; it does not include pool-acquire wait.",
        "statements": summarize_query_latencies(samples, calls).into_iter().map(|summary| serde_json::json!({
            "summary": summary.summary,
            "callCount": summary.call_count,
            "durationSampleCount": summary.duration_sample_count,
            "durationStatus": summary.duration_status,
            "cumulativeDurationUs": if summary.duration_sample_count > 0 { Some(summary.cumulative_duration_us) } else { None },
            "p50DurationUs": summary.p50_duration_us,
            "p95DurationUs": summary.p95_duration_us,
            "maxDurationUs": summary.max_duration_us,
        })).collect::<Vec<_>>(),
    })
}

fn performance_query_statement_counts() -> Arc<QueryStatementCounts> {
    static COUNTS: OnceLock<Arc<QueryStatementCounts>> = OnceLock::new();
    static INSTALLATION: OnceLock<Result<(), String>> = OnceLock::new();
    let counts = COUNTS
        .get_or_init(|| Arc::new(QueryStatementCounts::default()))
        .clone();
    let installation = INSTALLATION.get_or_init(|| {
        tracing_subscriber::registry()
            .with(QueryStatementLayer(counts.clone()))
            .try_init()
            .map_err(|error| error.to_string())
    });
    assert!(
        installation.is_ok(),
        "could not install SQLx statement counter: {installation:?}"
    );
    counts
}

struct PostgresLockWaitMonitor {
    stop: Arc<AtomicBool>,
    samples: Arc<AtomicUsize>,
    maximum_waiters: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

struct SqliteLockWaitMonitor {
    stop: Arc<AtomicBool>,
    waits_us: Arc<Mutex<Vec<u128>>>,
    errors: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

struct PoolPressureMonitor {
    stop: Arc<AtomicBool>,
    samples: Arc<AtomicUsize>,
    max_size: Arc<AtomicUsize>,
    max_idle: Arc<AtomicUsize>,
    max_in_use: Arc<AtomicUsize>,
    saturated_samples: Arc<AtomicUsize>,
    max_connections: usize,
    task: tokio::task::JoinHandle<()>,
}

fn pool_pressure_report(
    sample_count: usize,
    max_connections: usize,
    max_size: usize,
    max_idle: usize,
    max_in_use: usize,
    saturated_samples: usize,
) -> serde_json::Value {
    let has_samples = sample_count > 0;
    serde_json::json!({
        "status": if has_samples { "available" } else { "unavailable" },
        "sampleCount": sample_count,
        "unavailableReason": (!has_samples)
            .then_some("no pool snapshots were captured during this measurement window"),
        "maxConnections": max_connections,
        "maxSize": has_samples.then_some(max_size),
        "maxIdle": has_samples.then_some(max_idle),
        "maxInUse": has_samples.then_some(max_in_use),
        "saturatedSamples": has_samples.then_some(saturated_samples),
        "observedSaturated": has_samples.then_some(saturated_samples > 0),
    })
}

fn pool_pressure_measurement_report(
    sample_count: usize,
    max_connections: usize,
    max_size: usize,
    max_idle: usize,
    max_in_use: usize,
    saturated_samples: usize,
) -> serde_json::Value {
    serde_json::json!({
        "sampleIntervalMs": POOL_PRESSURE_SAMPLE_INTERVAL.as_millis(),
        "samplingNote": "5ms snapshots of in-memory pool counters; the sampler adds small scheduler/counter overhead, brief saturation between samples may be missed, and this does not directly measure pool-acquire wait.",
        "metrics": pool_pressure_report(
            sample_count,
            max_connections,
            max_size,
            max_idle,
            max_in_use,
            saturated_samples,
        ),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LocalPosterQueueSnapshot {
    batch_count: i64,
    completed_batch_count: i64,
    completed_with_image_marker_count: i64,
    pending_or_running_batch_count: i64,
    failed_batch_count: i64,
    cancelled_batch_count: i64,
    missing_image_marker_count: i64,
    poster_item_count: i64,
}

impl LocalPosterQueueSnapshot {
    fn is_drained(&self, required_poster_count: usize) -> bool {
        let required_poster_count = i64::try_from(required_poster_count).unwrap_or(i64::MAX);
        self.batch_count > 0
            && self.completed_batch_count == self.batch_count
            && self.completed_with_image_marker_count == self.batch_count
            && self.pending_or_running_batch_count == 0
            && self.failed_batch_count == 0
            && self.cancelled_batch_count == 0
            && self.missing_image_marker_count == 0
            && self.poster_item_count >= required_poster_count
    }

    fn has_completed_batch_missing_image_marker(&self) -> bool {
        self.completed_batch_count > self.completed_with_image_marker_count
    }

    fn is_pending_measurement_window(&self) -> bool {
        self.pending_or_running_batch_count > 0
            && self.failed_batch_count == 0
            && self.cancelled_batch_count == 0
            && !self.has_completed_batch_missing_image_marker()
    }

    fn pending_measurement_unavailable_reason(&self) -> &'static str {
        if self.cancelled_batch_count > 0 {
            "image queue contains cancelled batches, which are not pending work"
        } else if self.failed_batch_count > 0 {
            "image queue contains failed batches, which are not pending or running work"
        } else if self.has_completed_batch_missing_image_marker() {
            "a completed image batch is missing images_completed_at"
        } else {
            "no pending or running image batch existed at this sample boundary"
        }
    }

    fn report(&self, required_poster_count: usize) -> serde_json::Value {
        serde_json::json!({
            "batchCount": self.batch_count,
            "completedBatchCount": self.completed_batch_count,
            "completedWithImageMarkerCount": self.completed_with_image_marker_count,
            "pendingOrRunningBatchCount": self.pending_or_running_batch_count,
            "failedBatchCount": self.failed_batch_count,
            "cancelledBatchCount": self.cancelled_batch_count,
            "missingImageMarkerCount": self.missing_image_marker_count,
            "posterItemCount": self.poster_item_count,
            "requiredPosterItemCount": required_poster_count,
            "drained": self.is_drained(required_poster_count),
        })
    }
}

async fn load_local_poster_queue_snapshot(
    pool: &sqlx::AnyPool,
    job_id: &str,
    library_id: &str,
    job_id_placeholder: &str,
    library_id_placeholder: &str,
) -> Result<LocalPosterQueueSnapshot, sqlx::Error> {
    let query = format!(
        "SELECT batches.batch_count, batches.completed_batch_count,
                batches.completed_with_image_marker_count,
                batches.pending_or_running_batch_count, batches.failed_batch_count,
                batches.cancelled_batch_count, batches.missing_image_marker_count,
                posters.poster_item_count
         FROM (
             SELECT COUNT(*) AS batch_count,
                    COALESCE(SUM(CASE WHEN status = 'COMPLETED' THEN 1 ELSE 0 END), 0)
                        AS completed_batch_count,
                    COALESCE(SUM(CASE WHEN status = 'COMPLETED'
                                           AND images_completed_at IS NOT NULL
                                      THEN 1 ELSE 0 END), 0)
                        AS completed_with_image_marker_count,
                    COALESCE(SUM(CASE WHEN status IN ('PENDING', 'RUNNING')
                                      THEN 1 ELSE 0 END), 0)
                        AS pending_or_running_batch_count,
                    COALESCE(SUM(CASE WHEN status = 'FAILED' THEN 1 ELSE 0 END), 0)
                        AS failed_batch_count,
                    COALESCE(SUM(CASE WHEN status = 'CANCELLED' THEN 1 ELSE 0 END), 0)
                        AS cancelled_batch_count,
                    COALESCE(SUM(CASE WHEN images_completed_at IS NULL THEN 1 ELSE 0 END), 0)
                        AS missing_image_marker_count
             FROM scan_local_metadata_batches WHERE job_id = {job_id_placeholder}
         ) AS batches
         CROSS JOIN (
             SELECT COUNT(DISTINCT image.item_id) AS poster_item_count
             FROM item_images image
             JOIN media_items item ON item.id = image.item_id
             WHERE item.library_id = {library_id_placeholder}
               AND image.image_type = 'POSTER' AND image.source = 'LOCAL'
         ) AS posters"
    );
    let (
        batch_count,
        completed_batch_count,
        completed_with_image_marker_count,
        pending_or_running_batch_count,
        failed_batch_count,
        cancelled_batch_count,
        missing_image_marker_count,
        poster_item_count,
    ): (i64, i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(query))
        .bind(job_id)
        .bind(library_id)
        .fetch_one(pool)
        .await?;
    Ok(LocalPosterQueueSnapshot {
        batch_count,
        completed_batch_count,
        completed_with_image_marker_count,
        pending_or_running_batch_count,
        failed_batch_count,
        cancelled_batch_count,
        missing_image_marker_count,
        poster_item_count,
    })
}

fn start_pool_pressure_monitor(pool: sqlx::AnyPool) -> PoolPressureMonitor {
    let stop = Arc::new(AtomicBool::new(false));
    let samples = Arc::new(AtomicUsize::new(0));
    let max_size = Arc::new(AtomicUsize::new(0));
    let max_idle = Arc::new(AtomicUsize::new(0));
    let max_in_use = Arc::new(AtomicUsize::new(0));
    let saturated_samples = Arc::new(AtomicUsize::new(0));
    let max_connections = pool.options().get_max_connections() as usize;
    let monitor_stop = stop.clone();
    let monitor_samples = samples.clone();
    let monitor_max_size = max_size.clone();
    let monitor_max_idle = max_idle.clone();
    let monitor_max_in_use = max_in_use.clone();
    let monitor_saturated_samples = saturated_samples.clone();
    let task = tokio::spawn(async move {
        while !monitor_stop.load(Ordering::Relaxed) {
            let size = pool.size() as usize;
            let idle = pool.num_idle().min(size);
            let in_use = size.saturating_sub(idle);
            monitor_samples.fetch_add(1, Ordering::Relaxed);
            monitor_max_size.fetch_max(size, Ordering::Relaxed);
            monitor_max_idle.fetch_max(idle, Ordering::Relaxed);
            monitor_max_in_use.fetch_max(in_use, Ordering::Relaxed);
            if max_connections > 0 && size >= max_connections && idle == 0 {
                monitor_saturated_samples.fetch_add(1, Ordering::Relaxed);
            }
            tokio::time::sleep(POOL_PRESSURE_SAMPLE_INTERVAL).await;
        }
    });
    PoolPressureMonitor {
        stop,
        samples,
        max_size,
        max_idle,
        max_in_use,
        saturated_samples,
        max_connections,
        task,
    }
}

impl PoolPressureMonitor {
    async fn stop(self) -> serde_json::Value {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.task.await;
        pool_pressure_measurement_report(
            self.samples.load(Ordering::Relaxed),
            self.max_connections,
            self.max_size.load(Ordering::Relaxed),
            self.max_idle.load(Ordering::Relaxed),
            self.max_in_use.load(Ordering::Relaxed),
            self.saturated_samples.load(Ordering::Relaxed),
        )
    }
}

async fn measure_get_request_samples_with_pool_pressure(
    client: &reqwest::Client,
    pool: sqlx::AnyPool,
    url: &str,
    cookies: &str,
    label: &str,
) -> Result<(Vec<GetRequestSample>, serde_json::Value), Box<dyn std::error::Error>> {
    let monitor = start_pool_pressure_monitor(pool);
    let request_result = measure_get_request_samples_with_probe(
        client,
        url,
        cookies,
        label,
        FOREGROUND_REQUESTS,
        false,
    )
    .await;
    let pressure = monitor.stop().await;
    Ok((request_result?, pressure))
}

async fn measure_get_request_with_pool_pressure(
    client: &reqwest::Client,
    pool: sqlx::AnyPool,
    url: &str,
    cookies: &str,
    label: &str,
) -> Result<(GetRequestSample, serde_json::Value), Box<dyn std::error::Error>> {
    let monitor = start_pool_pressure_monitor(pool);
    let request_result =
        measure_get_request_samples_with_probe(client, url, cookies, label, 1, true)
            .await
            .and_then(|mut samples| {
                if samples.len() == 1 {
                    Ok(samples.remove(0))
                } else {
                    Err("single-request sample helper returned an unexpected count".into())
                }
            });
    let pressure = monitor.stop().await;
    Ok((request_result?, pressure))
}

fn start_sqlite_lock_wait_monitor(pool: sqlx::AnyPool) -> SqliteLockWaitMonitor {
    let stop = Arc::new(AtomicBool::new(false));
    let waits_us = Arc::new(Mutex::new(Vec::new()));
    let errors = Arc::new(AtomicUsize::new(0));
    let monitor_stop = stop.clone();
    let monitor_waits = waits_us.clone();
    let monitor_errors = errors.clone();
    let task = tokio::spawn(async move {
        while !monitor_stop.load(Ordering::Relaxed) {
            let started = Instant::now();
            match pool.begin_with("BEGIN IMMEDIATE").await {
                Ok(transaction) => {
                    let wait_us = started.elapsed().as_micros();
                    match monitor_waits.lock() {
                        Ok(mut waits) => waits.push(wait_us),
                        Err(poisoned) => poisoned.into_inner().push(wait_us),
                    }
                    if transaction.commit().await.is_err() {
                        monitor_errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(_) => {
                    let wait_us = started.elapsed().as_micros();
                    match monitor_waits.lock() {
                        Ok(mut waits) => waits.push(wait_us),
                        Err(poisoned) => poisoned.into_inner().push(wait_us),
                    }
                    monitor_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    SqliteLockWaitMonitor {
        stop,
        waits_us,
        errors,
        task,
    }
}

impl SqliteLockWaitMonitor {
    async fn stop(self) -> (Vec<u128>, usize) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.task.await;
        let waits = match self.waits_us.lock() {
            Ok(waits) => waits.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        (waits, self.errors.load(Ordering::Relaxed))
    }
}

fn start_postgres_lock_wait_monitor(pool: sqlx::AnyPool) -> PostgresLockWaitMonitor {
    let stop = Arc::new(AtomicBool::new(false));
    let samples = Arc::new(AtomicUsize::new(0));
    let maximum_waiters = Arc::new(AtomicUsize::new(0));
    let monitor_stop = stop.clone();
    let monitor_samples = samples.clone();
    let monitor_maximum_waiters = maximum_waiters.clone();
    let task = tokio::spawn(async move {
        while !monitor_stop.load(Ordering::Relaxed) {
            if let Ok(waiters) = sqlx::query_scalar::<_, i64>(
                "SELECT COUNT(*) FROM pg_stat_activity
                 WHERE datname = current_database()
                   AND wait_event_type = 'Lock'
                   AND pid <> pg_backend_pid()",
            )
            .fetch_one(&pool)
            .await
                && let Ok(waiters) = usize::try_from(waiters)
            {
                monitor_samples.fetch_add(1, Ordering::Relaxed);
                monitor_maximum_waiters.fetch_max(waiters, Ordering::Relaxed);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
    PostgresLockWaitMonitor {
        stop,
        samples,
        maximum_waiters,
        task,
    }
}

impl PostgresLockWaitMonitor {
    async fn stop(self) -> (usize, usize) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.task.await;
        (
            self.samples.load(Ordering::Relaxed),
            self.maximum_waiters.load(Ordering::Relaxed),
        )
    }
}

async fn postgres_wal_bytes(database: &Database) -> Result<u64, sqlx::Error> {
    let wal_bytes: String = sqlx::query_scalar("SELECT wal_bytes::text FROM pg_stat_wal")
        .fetch_one(database.pool())
        .await?;
    Ok(wal_bytes.parse().unwrap_or_default())
}

async fn configure_sqlite_benchmark_pragma(
    database: &Database,
    pragma: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool_connections = env::var("LUX_DB_MAX_CONNECTIONS")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(8);
    if !(1..=100).contains(&pool_connections) {
        return Err("LUX_DB_MAX_CONNECTIONS must be between 1 and 100".into());
    }
    let mut configured_connections = Vec::with_capacity(pool_connections);
    for _ in 0..pool_connections {
        let mut connection = database.pool().acquire().await?;
        sqlx::query(sqlx::AssertSqlSafe(pragma.clone()))
            .execute(&mut *connection)
            .await?;
        configured_connections.push(connection);
    }
    drop(configured_connections);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run with scripts/run-performance.sh for the LUX-045 ARM64 gate"]
async fn lux_045_catalog_scan_benchmark() -> Result<(), Box<dyn std::error::Error>> {
    let media_root = PathBuf::from(env::var("LUX_PERF_MEDIA_ROOT")?);
    let file_count: usize = env::var("LUX_PERF_FILE_COUNT")?.parse()?;
    assert!(file_count >= 60_000, "LUX-045 requires at least 60k files");
    assert!(media_root.join(".lux-fixture.json").is_file());
    let statement_counts = performance_query_statement_counts();

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
    let _loadgen_child_guard = IsolatedLoadgenChildGuard::new(&base_url)?;
    let client = reqwest::Client::builder()
        .no_proxy()
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
    statement_counts.reset();
    let first_started = Instant::now();
    let first = scanner.scan_movie_library(library.id).await?;
    let first_ms = first_started.elapsed().as_millis();
    let (first_scan_statement_count, first_scan_dml_count) = statement_counts.snapshot();
    assert_eq!(first.discovered_files, file_count);
    assert_eq!(first.created_items, file_count);
    assert_eq!(first.created_sources, file_count);

    if env::var_os("LUX_PERF_SCAN_ONLY").is_some() {
        println!(
            "LUX-045 DIRECT RESULT {}",
            serde_json::to_string(&json!({
                "commit": luxd::COMMIT,
                "architecture": std::env::consts::ARCH,
                "databaseBackend": "sqlite",
                "fileCount": file_count,
                "firstScanMs": first_ms,
                "discoveredFiles": first.discovered_files,
                "createdItems": first.created_items,
                "createdSources": first.created_sources,
                "sqlStatementCount": first_scan_statement_count,
                "dmlStatementCount": first_scan_dml_count,
            }))?
        );
        server.abort();
        return Ok(());
    }

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
    let incremental_fixture_paths = (60_000..60_000 + INCREMENTAL_FILES)
        .map(|index| {
            let year = 2000 + index % 100;
            incremental_directory.join(format!("Fixture.Movie.{index:06}.{year}.mkv"))
        })
        .collect::<Vec<_>>();
    for path in &incremental_fixture_paths {
        tokio::fs::write(path, b"LUX PERF INCREMENTAL FIXTURE\n").await?;
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
    for path in incremental_fixture_paths {
        tokio::fs::remove_file(path).await?;
    }

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
            "firstScanSqlStatementCount": first_scan_statement_count,
            "firstScanDmlStatementCount": first_scan_dml_count,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run with scripts/run-performance.sh for the LUX-270 Manifest job gate"]
async fn lux_270_manifest_job_scan_benchmark() -> Result<(), Box<dyn std::error::Error>> {
    let media_root = PathBuf::from(env::var("LUX_PERF_MEDIA_ROOT")?);
    let file_count: usize = env::var("LUX_PERF_FILE_COUNT")?.parse()?;
    assert!(
        file_count >= 100,
        "LUX-270 requires at least one full batch"
    );
    let fixture_manifest_path = media_root.join(".lux-fixture.json");
    assert!(fixture_manifest_path.is_file());
    let fixture_manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture_manifest_path)?)?;
    let directory_count = fixture_manifest["directoryCount"]
        .as_u64()
        .and_then(|count| usize::try_from(count).ok())
        .ok_or("fixture directoryCount is missing or invalid")?;

    let statement_counts = performance_query_statement_counts();
    let backend = env::var("LUX_PERF_BACKEND").unwrap_or_else(|_| "sqlite".to_owned());
    let library_kind_name =
        env::var("LUX_PERF_LIBRARY_KIND").unwrap_or_else(|_| "movie".to_owned());
    let library_kind = match library_kind_name.as_str() {
        "movie" => LibraryKind::Movie,
        "mixed" => LibraryKind::Mixed,
        unsupported => {
            return Err(format!(
                "unsupported LUX_PERF_LIBRARY_KIND {unsupported:?}; expected movie or mixed"
            )
            .into());
        }
    };
    let database_configuration = match backend.as_str() {
        "sqlite" => DatabaseConfiguration::Sqlite,
        "postgres" => {
            let database = match env::var("POSTGRES_TEST_DATABASE") {
                Ok(database)
                    if !database.is_empty()
                        && !matches!(database.as_str(), "postgres" | "template0" | "template1") =>
                {
                    database
                }
                _ => {
                    return Err(
                        "POSTGRES_TEST_DATABASE must name a disposable non-system database".into(),
                    );
                }
            };
            DatabaseConfiguration::Postgres(PostgresConnection {
                host: env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
                port: env::var("POSTGRES_TEST_PORT")
                    .unwrap_or_else(|_| "55432".to_owned())
                    .parse()?,
                database,
                username: env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
                password: env::var("POSTGRES_TEST_PASSWORD")
                    .unwrap_or_else(|_| "lux-test-password".to_owned()),
                ssl_mode: "disable".to_owned(),
            })
        }
        unsupported => {
            return Err(format!(
                "unsupported LUX_PERF_BACKEND {unsupported:?}; expected sqlite or postgres"
            )
            .into());
        }
    };
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(&config, &database_configuration).await?;
    if backend == "sqlite"
        && let Some(mode) = env::var_os("LUX_PERF_SQLITE_SYNCHRONOUS")
    {
        let mode = mode.to_string_lossy().to_ascii_uppercase();
        if !matches!(mode.as_str(), "OFF" | "NORMAL" | "FULL") {
            return Err(format!(
                "unsupported LUX_PERF_SQLITE_SYNCHRONOUS {mode:?}; expected OFF, NORMAL, or FULL"
            )
            .into());
        }
        sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA synchronous = {mode}")))
            .execute(database.pool())
            .await?;
    }
    if backend == "sqlite"
        && let Some(cache_kib) = env::var_os("LUX_PERF_SQLITE_CACHE_KIB")
    {
        let cache_kib = cache_kib
            .to_string_lossy()
            .parse::<usize>()
            .map_err(|_| "LUX_PERF_SQLITE_CACHE_KIB must be an integer")?;
        if !(512..=65_536).contains(&cache_kib) {
            return Err("LUX_PERF_SQLITE_CACHE_KIB must be between 512 and 65536".into());
        }
        configure_sqlite_benchmark_pragma(&database, format!("PRAGMA cache_size = -{cache_kib}"))
            .await?;
    }
    if backend == "sqlite"
        && let Some(checkpoint_pages) = env::var_os("LUX_PERF_SQLITE_WAL_AUTOCHECKPOINT_PAGES")
    {
        let checkpoint_pages = checkpoint_pages
            .to_string_lossy()
            .parse::<usize>()
            .map_err(|_| "LUX_PERF_SQLITE_WAL_AUTOCHECKPOINT_PAGES must be an integer")?;
        if checkpoint_pages > 1_000_000 {
            return Err("LUX_PERF_SQLITE_WAL_AUTOCHECKPOINT_PAGES must not exceed 1000000".into());
        }
        configure_sqlite_benchmark_pragma(
            &database,
            format!("PRAGMA wal_autocheckpoint = {checkpoint_pages}"),
        )
        .await?;
    }
    if backend == "sqlite"
        && let Some(temp_store) = env::var_os("LUX_PERF_SQLITE_TEMP_STORE")
    {
        let temp_store = temp_store.to_string_lossy().to_ascii_uppercase();
        if !matches!(temp_store.as_str(), "DEFAULT" | "FILE" | "MEMORY") {
            return Err("LUX_PERF_SQLITE_TEMP_STORE must be DEFAULT, FILE, or MEMORY".into());
        }
        configure_sqlite_benchmark_pragma(&database, format!("PRAGMA temp_store = {temp_store}"))
            .await?;
    }
    if backend == "sqlite"
        && let Some(mmap_size) = env::var_os("LUX_PERF_SQLITE_MMAP_SIZE")
    {
        let mmap_size = mmap_size
            .to_string_lossy()
            .parse::<u64>()
            .map_err(|_| "LUX_PERF_SQLITE_MMAP_SIZE must be an integer")?;
        if mmap_size > 1_073_741_824 {
            return Err("LUX_PERF_SQLITE_MMAP_SIZE must not exceed 1073741824".into());
        }
        configure_sqlite_benchmark_pragma(&database, format!("PRAGMA mmap_size = {mmap_size}"))
            .await?;
    }
    let sqlite_synchronous_level = if backend == "sqlite" {
        Some(
            sqlx::query_scalar::<_, i64>("PRAGMA synchronous")
                .fetch_one(database.pool())
                .await?,
        )
    } else {
        None
    };
    let sqlite_cache_size_pages = if backend == "sqlite" {
        Some(
            sqlx::query_scalar::<_, i64>("PRAGMA cache_size")
                .fetch_one(database.pool())
                .await?,
        )
    } else {
        None
    };
    let sqlite_wal_autocheckpoint_pages = if backend == "sqlite" {
        Some(
            sqlx::query_scalar::<_, i64>("PRAGMA wal_autocheckpoint")
                .fetch_one(database.pool())
                .await?,
        )
    } else {
        None
    };
    let sqlite_temp_store_mode = if backend == "sqlite" {
        Some(
            sqlx::query_scalar::<_, i64>("PRAGMA temp_store")
                .fetch_one(database.pool())
                .await?,
        )
    } else {
        None
    };
    let sqlite_mmap_size_bytes = if backend == "sqlite" {
        Some(
            sqlx::query_scalar::<_, i64>("PRAGMA mmap_size")
                .fetch_one(database.pool())
                .await?,
        )
    } else {
        None
    };
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("Admin", "Admin", "performance-only password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("LUX-270 Manifest Performance", library_kind, false)
        .await?;
    libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-utf8 fixture path")?,
        )
        .await?;
    let derived_index_triggers_disabled =
        env::var_os("LUX_PERF_DISABLE_DERIVED_INDEX_TRIGGERS").is_some();
    if derived_index_triggers_disabled {
        let triggers: &[(&str, &str)] = if backend == "postgres" {
            &[
                ("media_items_search_ai", "media_items"),
                ("media_items_search_au", "media_items"),
                ("media_items_search_ad", "media_items"),
                ("media_item_provider_ids_ai", "media_items"),
                ("media_item_provider_ids_au", "media_items"),
                ("media_sources_availability_ai", "media_sources"),
                ("media_sources_availability_au", "media_sources"),
                ("media_sources_availability_ad", "media_sources"),
                ("filesystem_entries_availability_au", "filesystem_entries"),
            ]
        } else {
            &[
                ("media_items_search_ai", "media_items"),
                ("media_items_search_au", "media_items"),
                ("media_items_search_ad", "media_items"),
                ("media_item_provider_ids_ai", "media_items"),
                ("media_item_provider_ids_au", "media_items"),
            ]
        };
        for (trigger, table) in triggers {
            let query = if backend == "postgres" {
                format!("DROP TRIGGER IF EXISTS {trigger} ON {table}")
            } else {
                format!("DROP TRIGGER IF EXISTS {trigger}")
            };
            sqlx::query(sqlx::AssertSqlSafe(query))
                .execute(database.pool())
                .await?;
        }
    }

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
    let _loadgen_child_guard = IsolatedLoadgenChildGuard::new(&base_url)?;
    let client = reqwest::Client::builder()
        .no_proxy()
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
    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let postgres_wal_before = if backend == "postgres" {
        Some(postgres_wal_bytes(&database).await?)
    } else {
        None
    };
    let sqlite_busy_timeout_ms = if backend == "sqlite" {
        Some(
            sqlx::query_scalar::<_, i64>("PRAGMA busy_timeout")
                .fetch_one(database.pool())
                .await?,
        )
    } else {
        None
    };
    statement_counts.reset();
    let manifest_pool_monitor = start_pool_pressure_monitor(database.pool().clone());
    let lock_monitor_enabled = env::var_os("LUX_PERF_DISABLE_LOCK_MONITOR").is_none();
    let postgres_lock_monitor = (backend == "postgres" && lock_monitor_enabled)
        .then(|| start_postgres_lock_wait_monitor(database.pool().clone()));
    let sqlite_lock_monitor = (backend == "sqlite" && lock_monitor_enabled)
        .then(|| start_sqlite_lock_wait_monitor(database.pool().clone()));

    let first_scan_started = Instant::now();
    let mut first_batch_durations = Vec::new();
    let mut first_scan_processed = 0_usize;
    let mut phase = "DISCOVERING".to_owned();
    let mut phase_durations = BTreeMap::<String, u128>::new();
    let mut phase_batch_counts = BTreeMap::<String, usize>::new();
    let job_id_placeholder = if backend == "postgres" { "$1" } else { "?" };
    loop {
        let batch_started = Instant::now();
        let report = jobs.run_batch(&job.id, 500).await?;
        let batch_duration = batch_started.elapsed().as_millis();
        first_batch_durations.push(batch_duration);
        *phase_durations.entry(phase.clone()).or_default() += batch_duration;
        *phase_batch_counts.entry(phase.clone()).or_default() += 1;
        first_scan_processed += report.processed;
        if report.completed {
            break;
        }
        if report.processed == 0 {
            let state_query =
                format!("SELECT state FROM scan_manifests WHERE job_id = {job_id_placeholder}");
            phase = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(state_query))
                .bind(&job.id)
                .fetch_one(database.pool())
                .await?;
        }
    }
    let manifest_index_ms = first_scan_started.elapsed().as_millis();
    let manifest_pool_pressure = manifest_pool_monitor.stop().await;
    let sqlite_wal_file_bytes_after_index = if backend == "sqlite" {
        fs::metadata(temp_dir.path().join("config/lux.db-wal"))
            .ok()
            .map(|metadata| metadata.len())
    } else {
        None
    };
    let manifest_stage_timings = statement_counts.scan_stage_values();
    let recorded_stages = stage_names(&manifest_stage_timings);
    for phase in [
        "directory_open",
        "directory_readdir",
        "directory_stat",
        "directory_batch_total",
        "baseline_query",
        "positive_classification",
        "positive_file_prepare",
        "positive_prepare_wall",
        "positive_file_recheck",
        "transaction_input_validation",
        "transaction_begin",
        "manifest_state_check",
        "directory_frontier_insert",
        "known_path_query",
        "directory_frontier_completion",
        "root_checkpoint",
        "manifest_observation_insert",
        "positive_index_apply",
        "positive_add_filesystem_claim",
        "movie_folder_refresh",
        "movie_item_prefetch",
        "movie_item_insert",
        "movie_source_insert",
        "provider_update",
        "derived_index",
        "positive_add_movie_materialization",
        "positive_add_episode_materialization",
        "positive_change_application",
        "positive_sidecar_target_registration",
        "presence_ledger",
        "manifest_counter_checkpoint",
        "transaction_commit",
        "transaction_total",
        "index_completion",
    ] {
        assert!(
            recorded_stages.contains(phase),
            "60k scan report is missing the {phase} stage; phase={phase}, processed={first_scan_processed}, writerCommits={}, writerAbort={}, writerCancelled={}, timing report: {manifest_stage_timings}",
            statement_counts.manifest_positive_commit_batch_count(),
            manifest_stage_timings["writerAbortFlagObserved"],
            manifest_stage_timings["writerCancelledObserved"]
        );
    }
    assert!(
        manifest_stage_timings["activePreparationTasksPeak"]
            .as_u64()
            .unwrap_or_default()
            > 0,
        "scan should report an observed preparation task peak"
    );
    assert_eq!(
        manifest_stage_timings["activeDirectoryReadersPeak"].as_u64(),
        Some(1),
        "sequential reader path should report one active directory reader"
    );
    assert_eq!(
        phase_batch_counts.get("APPLYING"),
        Some(&1),
        "a no-removal streamed scan should only finalize the apply phase once"
    );
    let (manifest_application_ms, manifest_transaction_ms, manifest_apply_timing_batches) =
        statement_counts.manifest_apply_timing_snapshot();
    let (manifest_preparation_concurrency, manifest_directory_read_concurrency) =
        statement_counts.manifest_directory_concurrency_snapshot();
    assert_eq!(manifest_directory_read_concurrency, 1);
    let (postgres_lock_wait_samples, postgres_max_lock_waiters) =
        if let Some(monitor) = postgres_lock_monitor {
            monitor.stop().await
        } else {
            (0, 0)
        };
    let (sqlite_lock_wait_us, sqlite_lock_wait_errors) = if let Some(monitor) = sqlite_lock_monitor
    {
        monitor.stop().await
    } else {
        (Vec::new(), 0)
    };
    let manifest_sql_latency_window = statement_counts.take_query_latency_values(
        "manifest_first_scan_loop",
        true,
        "SQLx events from the Manifest scan loop; SQLite lock-monitor statements may be included, while PostgreSQL lock-monitor SELECTs are excluded.",
    );
    let (raw_statement_count, dml_statement_count) = statement_counts.snapshot();
    let manifest_active_job_select_count = statement_counts.manifest_active_job_select_count();
    let dml_summary_counts = statement_counts.dml_summary_snapshot();
    let local_metadata_outbox_dml_count = dml_summary_counts
        .iter()
        .filter(|(_, summary)| summary.starts_with("INSERT INTO SCAN_LOCAL_METADATA_BATCHES"))
        .map(|(count, _)| count)
        .sum::<usize>();
    let scan_index_dml_count = dml_statement_count.saturating_sub(local_metadata_outbox_dml_count);
    let unclassified_cte_summaries = statement_counts.unclassified_cte_summary_snapshot();
    let lite_root_directory_state_updates = statement_counts
        .dml_statement_count_containing("UPDATE SCAN_MANIFEST_DIRECTORIES SET STATE");
    assert_eq!(
        lite_root_directory_state_updates, 1,
        "Lite discovery should mark its root directory complete once, after the in-memory frontier is empty"
    );
    assert_eq!(
        manifest_active_job_select_count, 0,
        "streamed positive commits should use the manifest state query for the active-job check"
    );
    let scan_statement_count = raw_statement_count.saturating_sub(postgres_lock_wait_samples);
    let scan_job_target_statement_count = dml_summary_counts
        .iter()
        .filter(|(_, summary)| summary.contains("INSERT INTO SCAN_JOB_TARGETS"))
        .map(|(count, _)| count)
        .sum::<usize>();
    let positive_commit_batch_count = statement_counts.manifest_positive_commit_batch_count();
    // Lite Manifest discovery submits at most 80 directories per work unit;
    // include those commits alongside the 8,000-file bound.
    let max_positive_commit_batch_count =
        file_count.div_ceil(8_000) + directory_count.div_ceil(80) + 5;
    assert!(
        positive_commit_batch_count <= max_positive_commit_batch_count,
        "streamed positive indexes should checkpoint no more than 8,000 files per transaction; transactions={positive_commit_batch_count}, upper_bound={max_positive_commit_batch_count}"
    );
    assert_eq!(
        scan_job_target_statement_count, 0,
        "index-completion measurement must not include postprocessing target materialization"
    );
    let media_item_insert_count = dml_summary_counts
        .iter()
        .filter(|(_, summary)| summary.starts_with("INSERT INTO MEDIA_ITEMS"))
        .map(|(count, _)| count)
        .sum::<usize>();
    let media_source_insert_count = dml_summary_counts
        .iter()
        .filter(|(_, summary)| summary.starts_with("INSERT INTO MEDIA_SOURCES"))
        .map(|(count, _)| count)
        .sum::<usize>();
    let filesystem_entry_insert_count = dml_summary_counts
        .iter()
        .filter(|(_, summary)| summary.starts_with("INSERT INTO FILESYSTEM_ENTRIES"))
        .map(|(count, _)| count)
        .sum::<usize>();
    assert_eq!(
        media_item_insert_count,
        media_source_insert_count + positive_commit_batch_count,
        "movie rows should batch alongside sources, with one parent-folder insert per positive batch"
    );
    let max_index_insert_statements = positive_commit_batch_count.saturating_mul(5);
    for (table, statement_count) in [
        ("media_sources", media_source_insert_count),
        ("filesystem_entries", filesystem_entry_insert_count),
    ] {
        assert!(
            statement_count <= max_index_insert_statements,
            "Manifest positive-index transactions should batch {table} inserts; got {statement_count} statements for {positive_commit_batch_count} transactions"
        );
    }
    assert!(
        media_item_insert_count <= positive_commit_batch_count.saturating_mul(6),
        "Manifest positive-index transactions should batch movie and parent-folder inserts; got {media_item_insert_count} statements for {positive_commit_batch_count} transactions"
    );
    for prefix in [
        "INSERT INTO SCAN_MANIFEST_DELTAS",
        "UPDATE SCAN_MANIFEST_DELTAS SET STATE",
        "UPDATE SCAN_MANIFESTS SET ADD_COUNT",
    ] {
        let statement_count = dml_summary_counts
            .iter()
            .filter(|(_, summary)| summary.starts_with(prefix))
            .map(|(count, _)| count)
            .sum::<usize>();
        assert_eq!(
            statement_count, 0,
            "positive streamed indexing must not persist per-file deltas: {prefix}"
        );
    }
    let max_scan_statement_count = file_count
        .saturating_mul(2)
        .saturating_add(directory_count.saturating_mul(3))
        .saturating_add(100);
    let max_scan_dml_count = file_count
        .saturating_add(directory_count.saturating_mul(3))
        .saturating_add(100);
    assert!(
        scan_statement_count <= max_scan_statement_count,
        "Manifest scan issued {scan_statement_count} SQL statements for {file_count} files; limit is {max_scan_statement_count}"
    );
    assert!(
        dml_statement_count <= max_scan_dml_count,
        "Manifest scan issued {dml_statement_count} DML statements for {file_count} files; limit is {max_scan_dml_count}"
    );
    if matches!(backend.as_str(), "sqlite" | "postgres") {
        assert!(
            dml_statement_count < 209,
            "LUX-275 backend batching should reduce total scan DML from the 209-statement baseline; backend={backend}, observed {dml_statement_count} including {local_metadata_outbox_dml_count} local metadata outbox statements and {scan_index_dml_count} scan-index statements"
        );
    }
    let postgres_wal_after = if backend == "postgres" {
        Some(postgres_wal_bytes(&database).await?)
    } else {
        None
    };
    let initial_manifest_query = format!(
        "SELECT state, observed_file_count, unchanged_count, add_count
         FROM scan_manifests WHERE job_id = {job_id_placeholder}"
    );
    let initial_manifest: (String, i64, i64, i64) =
        sqlx::query_as(sqlx::AssertSqlSafe(initial_manifest_query))
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(initial_manifest.0, "POSTPROCESSING");
    assert_eq!(initial_manifest.1, file_count as i64);
    assert_eq!(initial_manifest.3, file_count as i64);
    let initial_presence_query = format!(
        "SELECT manifest.discovery_format_version,
                (SELECT COUNT(*) FROM scan_manifest_seen_paths seen
                 WHERE seen.manifest_id = manifest.id),
                (SELECT COUNT(*) FROM scan_manifest_entries entry
                 WHERE entry.manifest_id = manifest.id AND entry.entry_kind = 'FILE'),
                (SELECT COUNT(*) FROM filesystem_entries entry
                 JOIN scan_manifest_roots root
                   ON root.manifest_id = manifest.id
                  AND root.library_root_id = entry.library_root_id
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 WHERE entry.entry_kind = 'FILE'
                   AND entry.last_seen_generation = job.generation)
         FROM scan_manifests manifest WHERE manifest.job_id = {job_id_placeholder}"
    );
    let initial_presence: (i64, i64, i64, i64) =
        sqlx::query_as(sqlx::AssertSqlSafe(initial_presence_query))
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(initial_presence, (3, 0, 0, file_count as i64));
    statement_counts.reset();
    let target_pool_monitor = start_pool_pressure_monitor(database.pool().clone());
    let target_materialization_started = Instant::now();
    jobs.materialize_manifest_postprocessing_targets(&job.id)
        .await?;
    let target_materialization_duration = target_materialization_started.elapsed();
    let target_materialization_pool_pressure = target_pool_monitor.stop().await;
    let target_materialization_sql_latency_window = statement_counts.take_query_latency_values(
        "target_materialization_call",
        true,
        "SQLx events observed during the target-materialization call; global instrumentation may include concurrent background SQL.",
    );
    let postprocessing_target_materialization_ms = target_materialization_duration.as_millis();
    tracing::debug!(
        target: "lux::scan_performance",
        phase = "target_materialization",
        duration_us = u64::try_from(target_materialization_duration.as_micros()).unwrap_or(u64::MAX),
        units = u64::try_from(file_count.saturating_mul(2)).unwrap_or(u64::MAX),
        files = u64::try_from(file_count).unwrap_or(u64::MAX),
        directories = u64::try_from(directory_count).unwrap_or(u64::MAX),
        "manifest target materialization timing"
    );
    let target_stage_timings = statement_counts.scan_stage_values();
    let (postprocessing_target_sql_count, postprocessing_target_dml_count) =
        statement_counts.snapshot();
    let target_insert_statement_count =
        statement_counts.dml_statement_count_containing("WITH PAGE_SOURCES AS MATERIALIZED");
    assert_eq!(
        target_insert_statement_count,
        file_count.div_ceil(32_000),
        "Lite target materialization should insert source and item targets in one statement per bounded 32k page"
    );
    let postprocessing_target_count_query =
        format!("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = {job_id_placeholder}");
    let postprocessing_target_count: i64 =
        sqlx::query_scalar(sqlx::AssertSqlSafe(postprocessing_target_count_query))
            .bind(&job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(postprocessing_target_count, (file_count * 2) as i64);
    let targets_ready_query = format!(
        "SELECT postprocessing_targets_ready FROM scan_manifests WHERE job_id = {job_id_placeholder}"
    );
    let targets_ready: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(targets_ready_query))
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(targets_ready, 1);

    statement_counts.reset();
    let catalog_page_url = format!(
        "{base_url}/api/v1/libraries/{}/items?page=1&pageSize=50",
        library.id
    );
    let (catalog_list_first_request_sample, catalog_list_first_request_pool_pressure) =
        measure_get_request_with_pool_pressure(
            &client,
            database.pool().clone(),
            &catalog_page_url,
            &cookies,
            "Manifest first catalog page",
        )
        .await?;
    let catalog_list_first_request_ms = catalog_list_first_request_sample.elapsed_ms;
    let catalog_list_first_request_sql_latency_window = statement_counts.take_query_latency_values(
        "catalog_list_first_request",
        true,
        "SQLx events observed during the single first catalog-page request; global instrumentation may include concurrent background SQL.",
    );
    let catalog_list_warmup_page_samples = measure_get_request_samples_with_probe(
        &client,
        &catalog_page_url,
        &cookies,
        "Manifest catalog connection warmup",
        FOREGROUND_REQUESTS,
        false,
    )
    .await?;
    let _catalog_list_warmup_sql_latency_window = statement_counts.take_query_latency_values(
        "catalog_list_connection_warmup",
        true,
        "SQLx events from the unmeasured 50-request batch that establishes the client connection pool before the warmed-page latency sample.",
    );
    let (catalog_list_warm_page_samples, catalog_list_warm_pool_pressure) =
        measure_get_request_samples_with_pool_pressure(
            &client,
            database.pool().clone(),
            &catalog_page_url,
            &cookies,
            "Manifest warmed catalog page",
        )
        .await?;
    let catalog_list_warm_page_ms = get_request_elapsed_ms(&catalog_list_warm_page_samples);
    let catalog_list_warm_sql_latency_window = statement_counts.take_query_latency_values(
        "catalog_list_warm_parallel_requests",
        true,
        "SQLx events observed during the 50 concurrent requests in this warmed catalog-page batch; global instrumentation may include concurrent background SQL.",
    );
    statement_counts.reset();
    let rescan_job = jobs.create_movie_scan_job(library.id).await?;
    let rescan_started = Instant::now();
    let first_rescan_batch = jobs.run_batch(&rescan_job.id, 500).await?;
    let mut rescan_batch_durations = vec![rescan_started.elapsed().as_millis()];
    assert!(
        !first_rescan_batch.completed,
        "the unchanged rescan fixture must span multiple batches"
    );
    let background_jobs = jobs.clone();
    let rescan_job_id = rescan_job.id.clone();
    let rescan_handle = tokio::spawn(async move {
        let mut processed = first_rescan_batch.processed;
        loop {
            let batch_started = Instant::now();
            let report = match background_jobs.run_batch(&rescan_job_id, 500).await {
                Ok(report) => report,
                Err(error) => return Err(error.to_string()),
            };
            rescan_batch_durations.push(batch_started.elapsed().as_millis());
            processed += report.processed;
            if report.completed {
                return Ok((processed, rescan_batch_durations));
            }
        }
    });
    tokio::task::yield_now().await;
    let scan_running_before_api = !rescan_handle.is_finished();
    let (foreground_samples, foreground_pool_pressure) =
        measure_get_request_samples_with_pool_pressure(
            &client,
            database.pool().clone(),
            &format!("{base_url}/api/v1/admin/libraries"),
            &cookies,
            "Manifest foreground",
        )
        .await?;
    let foreground_ms = get_request_elapsed_ms(&foreground_samples);
    let (catalog_list_samples, catalog_list_pool_pressure) =
        measure_get_request_samples_with_pool_pressure(
            &client,
            database.pool().clone(),
            &catalog_page_url,
            &cookies,
            "Manifest catalog list",
        )
        .await?;
    let catalog_list_ms = get_request_elapsed_ms(&catalog_list_samples);
    let (rescan_processed, rescan_batch_durations) = rescan_handle
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
        .map_err(std::io::Error::other)?;
    let rescan_ms = rescan_started.elapsed().as_millis();
    let unchanged_rescan_stage_timings = statement_counts.scan_stage_values();
    let unchanged_rescan_and_foreground_sql_latency_window =
        statement_counts.take_query_latency_values(
            "unchanged_rescan_plus_concurrent_foreground_requests",
            true,
            "SQLx events may include the unchanged rescan, admin API burst, catalog API burst, and SQLite lock monitor; PostgreSQL lock-monitor SELECTs are excluded.",
        );
    let batch_p50_ms = percentile(&first_batch_durations, 50);
    let batch_p95_ms = percentile(&first_batch_durations, 95);
    let foreground_p95_ms = percentile(&foreground_ms, 95);
    let catalog_list_p95_ms = percentile(&catalog_list_ms, 95);

    let rescan_manifest_query = format!(
        "SELECT state, observed_file_count, unchanged_count, add_count
         FROM scan_manifests WHERE job_id = {job_id_placeholder}"
    );
    let rescan_manifest: (String, i64, i64, i64) =
        sqlx::query_as(sqlx::AssertSqlSafe(rescan_manifest_query))
            .bind(&rescan_job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(rescan_manifest.0, "POSTPROCESSING");
    assert_eq!(
        rescan_manifest.1,
        file_count as i64,
        "rescan={rescan_manifest:?}, processed={rescan_processed}, batches={}, stages={}",
        rescan_batch_durations.len(),
        serde_json::to_string(&unchanged_rescan_stage_timings)?
    );
    assert_eq!(rescan_manifest.2, file_count as i64);
    assert_eq!(rescan_manifest.3, 0);
    let rescan_presence_query = format!(
        "SELECT discovery_format_version,
                (SELECT COUNT(*) FROM scan_manifest_seen_paths seen
                 WHERE seen.manifest_id = manifest.id),
                (SELECT COUNT(*) FROM filesystem_entries entry
                 JOIN scan_manifest_roots root
                   ON root.manifest_id = manifest.id
                  AND root.library_root_id = entry.library_root_id
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 WHERE entry.entry_kind = 'FILE'
                   AND entry.last_seen_generation = job.generation)
         FROM scan_manifests manifest WHERE manifest.job_id = {job_id_placeholder}"
    );
    let rescan_presence: (i64, i64, i64) =
        sqlx::query_as(sqlx::AssertSqlSafe(rescan_presence_query))
            .bind(&rescan_job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(rescan_presence, (3, 0, file_count as i64));
    jobs.materialize_manifest_postprocessing_targets(&rescan_job.id)
        .await?;
    assert!(
        scan_running_before_api,
        "Manifest scan ended before foreground sampling"
    );
    let manifest_presence = json!({
        "discoveryFormatVersion": initial_presence.0,
        "seenPathCount": initial_presence.1,
        "fileObservationCount": initial_presence.2,
        "generationMarkedPathCount": initial_presence.3,
        "rescanDiscoveryFormatVersion": rescan_presence.0,
        "rescanSeenPathCount": rescan_presence.1,
        "rescanGenerationMarkedPathCount": rescan_presence.2,
    });
    let process_peak_rss_bytes = process_peak_rss_bytes();

    let report_sections = [
        json!({
            "commit": luxd::COMMIT,
            "architecture": std::env::consts::ARCH,
            "processPeakRssBytes": process_peak_rss_bytes,
            "databaseBackend": backend,
            "scanDiscoveryStrategy": "lite_grouped",
            "derivedIndexTriggersDisabled": derived_index_triggers_disabled,
            "libraryKind": library_kind_name,
            "fileCount": file_count,
            "manifestIndexMs": manifest_index_ms,
            "manifestFirstScanSqlLatencyWindow": manifest_sql_latency_window,
            "manifestPoolPressure": manifest_pool_pressure,
            "manifestFilesProcessed": first_scan_processed,
            "manifestBatchCount": first_batch_durations.len(),
            "manifestPhaseMs": phase_durations,
            "manifestPhaseBatchCounts": phase_batch_counts,
            "manifestStageTimings": manifest_stage_timings,
            "manifestPositivePreparationMs": manifest_application_ms,
            "manifestPositiveCommitMs": manifest_transaction_ms,
            "manifestPositiveTimingEventCount": manifest_apply_timing_batches,
            "manifestPositiveCommitBatchCount": positive_commit_batch_count,
            "manifestActiveJobSelectCount": manifest_active_job_select_count,
            "manifestPreparationConcurrency": manifest_preparation_concurrency,
            "manifestDirectoryReadConcurrency": manifest_directory_read_concurrency,
            "postprocessingTargetMaterializationMs": postprocessing_target_materialization_ms,
            "targetMaterializationSqlLatencyWindow": target_materialization_sql_latency_window,
            "targetMaterializationPoolPressure": target_materialization_pool_pressure,
            "targetStageTimings": target_stage_timings,
            "postprocessingTargetSqlStatementCount": postprocessing_target_sql_count,
            "postprocessingTargetDmlStatementCount": postprocessing_target_dml_count,
            "postprocessingTargetInsertStatementCount": target_insert_statement_count,
            "postprocessingTargetCount": postprocessing_target_count,
            "batchP50Ms": batch_p50_ms,
            "batchP95Ms": batch_p95_ms,
        }),
        json!({
            "manifestSqlStatementCount": scan_statement_count,
            "manifestDmlStatementCount": dml_statement_count,
            "manifestScanIndexDmlStatementCount": scan_index_dml_count,
            "manifestLocalMetadataOutboxDmlStatementCount": local_metadata_outbox_dml_count,
            "manifestDmlSummaryCounts": dml_summary_counts,
            "unclassifiedCteSummaries": unclassified_cte_summaries,
            "postgresWalBytesWritten": postgres_wal_before
                .zip(postgres_wal_after)
                .map(|(before, after)| after.saturating_sub(before)),
            "postgresLockWaitSampleCount": (backend == "postgres")
                .then_some(postgres_lock_wait_samples),
            "postgresMaxObservedLockWaiters": (backend == "postgres")
                .then_some(postgres_max_lock_waiters),
            "sqliteLockMonitorEnabled": (backend == "sqlite").then_some(lock_monitor_enabled),
            "sqliteLockAcquisitionSampleCount": (backend == "sqlite" && lock_monitor_enabled)
                .then_some(sqlite_lock_wait_us.len()),
            "sqliteLockAcquisitionP50Us": (backend == "sqlite" && lock_monitor_enabled)
                .then(|| percentile(&sqlite_lock_wait_us, 50)),
            "sqliteLockAcquisitionP95Us": (backend == "sqlite" && lock_monitor_enabled)
                .then(|| percentile(&sqlite_lock_wait_us, 95)),
            "sqliteLockAcquisitionMaxUs": (backend == "sqlite" && lock_monitor_enabled)
                .then_some(sqlite_lock_wait_us.iter().copied().max().unwrap_or_default()),
            "sqliteLockAcquisitionErrors": (backend == "sqlite" && lock_monitor_enabled)
                .then_some(sqlite_lock_wait_errors),
            "sqliteBusyTimeoutMs": sqlite_busy_timeout_ms,
            "sqliteSynchronousLevel": sqlite_synchronous_level,
            "sqliteCacheSizePages": sqlite_cache_size_pages,
            "sqliteWalAutocheckpointPages": sqlite_wal_autocheckpoint_pages,
            "sqliteWalFileBytesAfterIndex": sqlite_wal_file_bytes_after_index,
            "sqliteTempStoreMode": sqlite_temp_store_mode,
            "sqliteMmapSizeBytes": sqlite_mmap_size_bytes,
        }),
        json!({
            "foregroundDuringScan": scan_running_before_api,
            "foregroundRequestCount": FOREGROUND_REQUESTS,
            "foregroundP95Ms": foreground_p95_ms,
            "foregroundHttpRequestProfile": get_request_profile(&foreground_samples),
            "foregroundPoolPressure": foreground_pool_pressure,
            "catalogListP95Ms": catalog_list_p95_ms,
            "catalogListHttpRequestProfile": get_request_profile(&catalog_list_samples),
            "catalogListPoolPressure": catalog_list_pool_pressure,
            "catalogListFirstRequestMs": catalog_list_first_request_ms,
            "catalogListFirstRequestPoolPressure": catalog_list_first_request_pool_pressure,
            "catalogListFirstRequestHttpRequestProfile":
                get_request_profile(std::slice::from_ref(&catalog_list_first_request_sample)),
            "catalogListFirstRequestSqlLatencyWindow":
                catalog_list_first_request_sql_latency_window,
            "catalogListWarmupRequestCount": catalog_list_warmup_page_samples.len(),
            "catalogListWarmupHttpRequestProfile":
                get_request_profile(&catalog_list_warmup_page_samples),
            "catalogListWarmP50Ms": percentile(&catalog_list_warm_page_ms, 50),
            "catalogListWarmP95Ms": percentile(&catalog_list_warm_page_ms, 95),
            "catalogListWarmRequestCount": catalog_list_warm_page_ms.len(),
            "catalogListWarmHttpRequestProfile":
                get_request_profile(&catalog_list_warm_page_samples),
            "catalogListWarmPoolPressure": catalog_list_warm_pool_pressure,
            "catalogListWarmPageSqlLatencyWindow": catalog_list_warm_sql_latency_window,
            "unchangedRescanMs": rescan_ms,
            "unchangedRescanAndConcurrentForegroundSqlLatencyWindow": unchanged_rescan_and_foreground_sql_latency_window,
            "unchangedRescanStageTimings": unchanged_rescan_stage_timings,
            "unchangedRescanBatchCount": rescan_batch_durations.len(),
            "unchangedRescanProcessed": rescan_processed,
            "manifestState": initial_manifest.0,
            "manifestObservedFiles": initial_manifest.1,
            "manifestAddedFiles": initial_manifest.3,
            "manifestPresence": manifest_presence,
            "sqlStatementCountNote": "SQLx statement events counted; PostgreSQL pg_stat_activity monitor SELECTs excluded. SQLite lock monitor acquires BEGIN IMMEDIATE every 100ms and commits immediately; its statements are included if surfaced by SQLx instrumentation. DML count classifies INSERT/UPDATE/DELETE/REPLACE/TRUNCATE summaries, including common-table-expression writes.",
            "queryLatencyNote": "SQLx elapsed_secs summaries are grouped by normalized SQLx statement summary (first four SQL tokens; bind values and full SQL are not captured). PostgreSQL pg_stat_activity lock-monitor SELECTs are excluded; SQLite lock-monitor BEGIN IMMEDIATE/COMMIT statements may appear if SQLx emits elapsed_secs for them.",
            "poolPressureNote": "Pool snapshots use 5ms in-memory samples during each labeled measurement; they can miss shorter saturation intervals and do not directly measure acquire wait."
        }),
    ];
    let mut report = serde_json::Map::new();
    for section in report_sections {
        report.extend(
            section
                .as_object()
                .ok_or("performance result section must be an object")?
                .clone(),
        );
    }
    println!(
        "LUX-270 MANIFEST RESULT {}",
        serde_json::to_string(&serde_json::Value::Object(report))?
    );

    server.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "run with scripts/run-performance.sh for the LUX-304 progressive poster gate"]
async fn lux_304_progressive_poster_worker_benchmark() -> Result<(), Box<dyn std::error::Error>> {
    let media_root = PathBuf::from(env::var("LUX_PERF_MEDIA_ROOT")?);
    let file_count: usize = env::var("LUX_PERF_FILE_COUNT")?.parse()?;
    assert!(
        file_count >= 100,
        "LUX-304 requires at least one full batch"
    );
    let fixture_manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(media_root.join(".lux-fixture.json"))?)?;
    let directory_count = fixture_manifest["directoryCount"]
        .as_u64()
        .and_then(|count| usize::try_from(count).ok())
        .ok_or("fixture directoryCount is missing or invalid")?;
    let files_per_directory = file_count.div_ceil(directory_count);
    for index in 0..file_count {
        let bucket = index / files_per_directory;
        let year = 2000 + index % 100;
        let media_stem = format!("Fixture.Movie.{index:06}.{year}");
        let poster_path = media_root
            .join(format!("bucket-{bucket:04}"))
            .join(format!("{media_stem}-poster.png"));
        fs::write(poster_path, METADATA_BENCHMARK_PNG)?;
    }

    let backend = env::var("LUX_PERF_BACKEND").unwrap_or_else(|_| "sqlite".to_owned());
    let database_configuration = match backend.as_str() {
        "sqlite" => DatabaseConfiguration::Sqlite,
        "postgres" => {
            let database = match env::var("POSTGRES_TEST_DATABASE") {
                Ok(database)
                    if !database.is_empty()
                        && !matches!(database.as_str(), "postgres" | "template0" | "template1") =>
                {
                    database
                }
                _ => {
                    return Err(
                        "POSTGRES_TEST_DATABASE must name a disposable non-system database".into(),
                    );
                }
            };
            DatabaseConfiguration::Postgres(PostgresConnection {
                host: env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
                port: env::var("POSTGRES_TEST_PORT")
                    .unwrap_or_else(|_| "55432".to_owned())
                    .parse()?,
                database,
                username: env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
                password: env::var("POSTGRES_TEST_PASSWORD")
                    .unwrap_or_else(|_| "lux-test-password".to_owned()),
                ssl_mode: "disable".to_owned(),
            })
        }
        unsupported => {
            return Err(format!(
                "unsupported LUX_PERF_BACKEND {unsupported:?}; expected sqlite or postgres"
            )
            .into());
        }
    };
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(&config, &database_configuration).await?;
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("Admin", "Admin", "performance-only password")
        .await?;
    let job_id_placeholder = if backend == "postgres" { "$1" } else { "?" };
    let library_id_placeholder = if backend == "postgres" { "$1" } else { "?" };
    let queue_library_id_placeholder = if backend == "postgres" { "$2" } else { "?" };
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("LUX-304 Progressive Posters", LibraryKind::Movie, false)
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
        config.clone(),
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let _loadgen_child_guard = IsolatedLoadgenChildGuard::new(&base_url)?;
    let client = reqwest::Client::builder()
        .no_proxy()
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
    let csrf_token = cookie_value(login.headers(), "lux_csrf");
    let cookies = format!(
        "lux_session={}; lux_csrf={csrf_token}",
        cookie_value(login.headers(), "lux_session")
    );
    let catalog_connection_warmup_samples = measure_get_request_samples_with_probe(
        &client,
        &format!("{base_url}/api/v1/libraries"),
        &cookies,
        "LUX-304 catalog connection warmup",
        FOREGROUND_REQUESTS,
        false,
    )
    .await?;
    let scan_started = Instant::now();
    let observation_database = database.clone();
    let observation_library_id = library.id.to_string();
    let observation_started = scan_started;
    let (local_poster_indexed_sender, local_poster_indexed_receiver) =
        tokio::sync::oneshot::channel();
    let poster_position_query = format!(
        "WITH ranked_movies AS (
             SELECT item.id,
                    ROW_NUMBER() OVER (ORDER BY LOWER(item.title) ASC, item.id ASC) AS position
             FROM media_items item
             WHERE item.library_id = {library_id_placeholder}
               AND item.item_type = 'MOVIE'
               AND item.removed_at IS NULL
               AND item.merged_into_item_id IS NULL
               AND item.has_available_source = 1
         )
         SELECT ranked_movies.id, ranked_movies.position
         FROM ranked_movies
         JOIN item_images image ON image.item_id = ranked_movies.id
         WHERE image.image_type = 'POSTER' AND image.source = 'LOCAL'
         ORDER BY ranked_movies.position
         LIMIT 1"
    );
    let first_visibility_handle = tokio::spawn(async move {
        let mut first_item_visible_ms = None;
        let mut first_poster_indexed_ms = None;
        let mut local_poster_indexed_sender = Some(local_poster_indexed_sender);
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            if first_item_visible_ms.is_none()
                && sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(format!(
                    "SELECT id FROM media_items
                     WHERE library_id = {library_id_placeholder} AND item_type = 'MOVIE'
                       AND removed_at IS NULL AND merged_into_item_id IS NULL
                       AND has_available_source = 1
                     ORDER BY LOWER(title), id LIMIT 1"
                )))
                .bind(&observation_library_id)
                .fetch_optional(observation_database.pool())
                .await?
                .is_some()
            {
                first_item_visible_ms = Some(observation_started.elapsed().as_millis());
            }
            if first_poster_indexed_ms.is_none()
                && let Some((poster_item_id, movie_position)) = sqlx::query_as::<_, (String, i64)>(
                    sqlx::AssertSqlSafe(poster_position_query.clone()),
                )
                .bind(&observation_library_id)
                .fetch_optional(observation_database.pool())
                .await?
            {
                if let Some(poster_page) = catalog_poster_page_for_movie_position(movie_position) {
                    first_poster_indexed_ms = Some(observation_started.elapsed().as_millis());
                    if let Some(sender) = local_poster_indexed_sender.take() {
                        let _ = sender.send((poster_item_id, poster_page));
                    }
                }
            }
            if let (Some(first_item_visible_ms), Some(first_poster_indexed_ms)) =
                (first_item_visible_ms, first_poster_indexed_ms)
            {
                return Ok::<_, sqlx::Error>((first_item_visible_ms, first_poster_indexed_ms));
            }
            if Instant::now() >= deadline {
                return Err(sqlx::Error::Protocol(
                    "LUX-304 first item/poster visibility timed out".to_owned(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    let api_client = client.clone();
    let catalog_list_url = format!(
        "{base_url}/api/v1/libraries/{}/items?page=1&pageSize=50",
        library.id
    );
    let api_url = catalog_list_url.clone();
    let api_cookies = cookies.clone();
    let (scan_completed_sender, scan_completed_receiver) = tokio::sync::oneshot::channel();
    let poster_probe_client = client.clone();
    let poster_probe_catalog_url = format!(
        "{base_url}/api/v1/libraries/{}/items?itemType=MOVIE&sortBy=Name&sortOrder=Ascending&pageSize={POSTER_CATALOG_PAGE_SIZE}",
        library.id,
    );
    let poster_probe_base_url = base_url.clone();
    let poster_probe_cookies = cookies.clone();
    let poster_probe_database = database.clone();
    let poster_probe_library_id = library.id.to_string();
    let poster_probe_library_id_placeholder = library_id_placeholder;
    let poster_api_probe_handle = tokio::spawn(async move {
        probe_catalog_poster_until_scan_completes(
            poster_probe_client,
            poster_probe_catalog_url,
            poster_probe_base_url,
            poster_probe_cookies,
            poster_probe_database,
            poster_probe_library_id,
            poster_probe_library_id_placeholder,
            scan_started,
            local_poster_indexed_receiver,
            scan_completed_receiver,
        )
        .await
    });
    let scan_start_response = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{}/scan",
            library.id
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_token)
        .send()
        .await?;
    assert_eq!(scan_start_response.status(), reqwest::StatusCode::ACCEPTED);
    let scan_start_body = scan_start_response.json::<serde_json::Value>().await?;
    let scan_job_id = scan_start_body
        .pointer("/job/id")
        .and_then(serde_json::Value::as_str)
        .ok_or("admin scan endpoint omitted job.id")?
        .to_owned();
    let scan_job_state_query =
        format!("SELECT status, scan_phase FROM scan_jobs WHERE id = {job_id_placeholder}");
    let initial_scan_job_state =
        sqlx::query_as::<_, (String, String)>(sqlx::AssertSqlSafe(scan_job_state_query.clone()))
            .bind(&scan_job_id)
            .fetch_one(database.pool())
            .await?;
    let scan_active_at_api_start =
        scan_job_state_is_active(&initial_scan_job_state.0, &initial_scan_job_state.1);
    let monitor_database = database.clone();
    let monitor_job_id = scan_job_id.clone();
    let monitor_state_query = scan_job_state_query.clone();
    let monitor_started = scan_started;
    let scan_completion_handle = tokio::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            let state = sqlx::query_as::<_, (String, String)>(sqlx::AssertSqlSafe(
                monitor_state_query.clone(),
            ))
            .bind(&monitor_job_id)
            .fetch_optional(monitor_database.pool())
            .await?;
            match state.as_ref() {
                Some((job_status, scan_phase))
                    if job_status == "COMPLETED" && scan_phase == "IDLE" =>
                {
                    return Ok::<_, sqlx::Error>(monitor_started.elapsed().as_millis());
                }
                Some((job_status, _)) if matches!(job_status.as_str(), "FAILED" | "CANCELLED") => {
                    return Err(sqlx::Error::Protocol(format!(
                        "LUX-304 scan job reached terminal status {state:?}"
                    )));
                }
                Some(_) => {}
                None => {
                    return Err(sqlx::Error::Protocol(
                        "LUX-304 scan job disappeared before completion".to_owned(),
                    ));
                }
            }
            if Instant::now() >= deadline {
                return Err(sqlx::Error::Protocol(
                    "LUX-304 scan job completion timed out".to_owned(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    let catalog_list_handle = tokio::spawn(async move {
        measure_get_request_samples_with_probe(
            &api_client,
            &api_url,
            &api_cookies,
            "LUX-304 progressive catalog list",
            FOREGROUND_REQUESTS,
            true,
        )
        .await
        .map_err(|error| error.to_string())
    });
    let scan_job_completion_ms = scan_completion_handle
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))??;
    let _ = scan_completed_sender.send(());
    let poster_api_probe = poster_api_probe_handle
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
        .map_err(std::io::Error::other)?;
    let scan_finished_queue_snapshot = load_local_poster_queue_snapshot(
        database.pool(),
        &scan_job_id,
        &library.id.to_string(),
        job_id_placeholder,
        queue_library_id_placeholder,
    )
    .await?;
    let (first_item_visible_ms, first_poster_indexed_ms) = first_visibility_handle
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))??;
    let catalog_list_samples = catalog_list_handle
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
        .map_err(std::io::Error::other)?;
    let catalog_list_http_profile = get_request_profile(&catalog_list_samples);
    let catalog_list_ms = catalog_list_samples
        .iter()
        .map(|sample| sample.elapsed_ms)
        .collect::<Vec<_>>();
    let catalog_list_p95_ms = percentile(&catalog_list_ms, 95);

    let require_detached_poster_queue = env::var("LUX_PERF_REQUIRE_DETACHED_POSTER_QUEUE")
        .map(|value| value != "0")
        .unwrap_or(true);
    let scan_finished_image_pending = scan_finished_queue_snapshot.is_pending_measurement_window();
    let catalog_list_after_scan_image_pending_ms = if scan_finished_image_pending {
        Some(
            measure_get_requests(
                &client,
                &catalog_list_url,
                &cookies,
                "LUX-304 catalog list after scan while images are pending",
            )
            .await?,
        )
    } else {
        None
    };
    let scan_finished_queue_after_api_snapshot = load_local_poster_queue_snapshot(
        database.pool(),
        &scan_job_id,
        &library.id.to_string(),
        job_id_placeholder,
        queue_library_id_placeholder,
    )
    .await?;
    let scan_finished_image_pending_after_requests =
        scan_finished_queue_after_api_snapshot.is_pending_measurement_window();
    let scan_finished_image_pending_p95_ms = catalog_list_after_scan_image_pending_ms
        .as_deref()
        .filter(|_| scan_finished_image_pending && scan_finished_image_pending_after_requests)
        .map(|samples| percentile(samples, 95));
    let scan_finished_image_pending_unavailable_reason =
        scan_finished_image_pending_p95_ms.is_none().then(|| {
            if scan_finished_image_pending {
                scan_finished_queue_after_api_snapshot
                    .pending_measurement_unavailable_reason()
                    .to_owned()
            } else {
                scan_finished_queue_snapshot
                    .pending_measurement_unavailable_reason()
                    .to_owned()
            }
        });

    let poster_queue_deadline = Instant::now() + Duration::from_secs(600);
    let (local_poster_queue_ms, local_poster_queue_final_snapshot) = loop {
        let snapshot = load_local_poster_queue_snapshot(
            database.pool(),
            &scan_job_id,
            &library.id.to_string(),
            job_id_placeholder,
            queue_library_id_placeholder,
        )
        .await?;
        if snapshot.has_completed_batch_missing_image_marker() {
            return Err(format!(
                "LUX-304 completed image batch is missing images_completed_at and cannot be considered drained: {}",
                snapshot.report(file_count)
            )
            .into());
        }
        if snapshot.cancelled_batch_count > 0 {
            return Err(format!(
                "LUX-304 image queue contains cancelled batches and cannot be considered drained: {}",
                snapshot.report(file_count)
            )
            .into());
        }
        if snapshot.is_drained(file_count) {
            break (scan_started.elapsed().as_millis(), snapshot);
        }
        if Instant::now() >= poster_queue_deadline {
            return Err(format!(
                "LUX-304 strict local image queue drain timed out: {}",
                snapshot.report(file_count)
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let catalog_list_after_queue_samples = measure_get_request_samples_with_probe(
        &client,
        &catalog_list_url,
        &cookies,
        "LUX-304 catalog list after local posters",
        FOREGROUND_REQUESTS,
        true,
    )
    .await?;
    let catalog_list_after_queue_ms = get_request_elapsed_ms(&catalog_list_after_queue_samples);
    let catalog_list_after_queue_p95_ms = percentile(&catalog_list_after_queue_ms, 95);
    let catalog_list_after_queue_http_profile =
        get_request_profile(&catalog_list_after_queue_samples);
    let online_fill_missing_job_count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM metadata_reidentify_jobs
         WHERE library_id = {library_id_placeholder} AND mode = 'FILL_MISSING'"
    )))
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(online_fill_missing_job_count, 0);
    assert!(
        scan_active_at_api_start,
        "scan job was not active after the admin scan API returned: status={:?}, phase={:?}",
        initial_scan_job_state.0, initial_scan_job_state.1
    );
    assert!(first_item_visible_ms <= scan_job_completion_ms);
    assert!(
        first_poster_indexed_ms <= scan_job_completion_ms,
        "first local poster was indexed at {first_poster_indexed_ms} ms, after scan completion at {scan_job_completion_ms} ms"
    );
    let poster_api_visible_before_scan_completion = poster_api_probe
        .poster_visible_ms
        .is_some_and(|elapsed_ms| elapsed_ms <= scan_job_completion_ms);
    if require_detached_poster_queue {
        assert!(
            poster_api_visible_before_scan_completion,
            "a poster visible through the authenticated catalog and poster APIs was not returned before scan completion: probe={poster_api_probe:?}, first item visible={first_item_visible_ms} ms, first poster indexed={first_poster_indexed_ms} ms, scan completion={scan_job_completion_ms} ms"
        );
    }
    if require_detached_poster_queue {
        assert!(
            scan_job_completion_ms < local_poster_queue_ms,
            "the scan job must return before all image batches and local posters are drained"
        );
    }
    println!(
        "LUX-304 POSTER RESULT {}",
        serde_json::to_string(&json!({
            "commit": luxd::COMMIT,
            "architecture": std::env::consts::ARCH,
            "databaseBackend": backend,
            "fileCount": file_count,
            "directoryCount": directory_count,
            "posterQueueMode": "detached_batches",
            "scanJobCompletionMs": scan_job_completion_ms,
            "firstItemVisibleMs": first_item_visible_ms,
            "firstPosterIndexedMs": first_poster_indexed_ms,
            "catalogPosterApiProbe": {
                "probeIntervalMs": POSTER_API_PROBE_INTERVAL.as_millis(),
                "maxCatalogListRequests": POSTER_API_PROBE_MAX_REQUESTS,
                "maxProbeDurationMs": POSTER_API_PROBE_MAX_DURATION.as_millis(),
                "requestTimeoutMs": POSTER_API_PROBE_REQUEST_TIMEOUT.as_millis(),
                "localPosterIndexObserved": poster_api_probe.local_poster_index_observed,
                "catalogPage": poster_api_probe.catalog_page,
                "catalogItemVisibleMs": poster_api_probe.catalog_item_visible_ms,
                "catalogRankLookupCount": poster_api_probe.catalog_rank_lookup_count,
                "catalogListRequestCount": poster_api_probe.catalog_list_request_count,
                "posterEndpointRequestCount": poster_api_probe.poster_endpoint_request_count,
                "itemId": poster_api_probe.item_id,
                "posterTag": poster_api_probe.poster_tag,
                "posterTagVisibleMs": poster_api_probe.poster_tag_visible_ms,
                "posterVisibleMs": poster_api_probe.poster_visible_ms,
                "posterEndpointLatencyMs": poster_api_probe.poster_endpoint_latency_ms,
                "contentType": poster_api_probe.content_type,
                "bodyBytes": poster_api_probe.body_bytes,
                "bodyDecodedAsImage": poster_api_probe.body_decoded_as_image,
                "lastError": poster_api_probe.last_error,
                "posterVisibleBeforeScanCompletion": poster_api_visible_before_scan_completion,
                "backgroundTrafficDuringScan": true,
                "includedInCatalogListP95": false,
            },
            "catalogListP95DuringScanMs": catalog_list_p95_ms,
            "catalogListHttpRequestProfile": catalog_list_http_profile,
            "catalogConnectionWarmupRequestCount": catalog_connection_warmup_samples.len(),
            "catalogConnectionWarmupHttpRequestProfile":
                get_request_profile(&catalog_connection_warmup_samples),
            "catalogListRequestCount": catalog_list_ms.len(),
            "catalogListP95ScanFinishedImagePendingMs": scan_finished_image_pending_p95_ms,
            "catalogListScanFinishedImagePendingSampleStatus":
                if scan_finished_image_pending_p95_ms.is_some() {
                    "available"
                } else {
                    "unavailable"
                },
            "catalogListScanFinishedImagePendingUnavailableReason":
                scan_finished_image_pending_unavailable_reason,
            "catalogListScanFinishedImagePendingRequestCount":
                catalog_list_after_scan_image_pending_ms
                    .as_ref()
                    .map(Vec::len)
                    .unwrap_or_default(),
            "scanFinishedImageQueueSnapshotAtRequestStart":
                scan_finished_queue_snapshot.report(file_count),
            "scanFinishedImageQueueSnapshotAfterRequestBatch":
                scan_finished_queue_after_api_snapshot.report(file_count),
            "catalogListP95AfterPosterQueueMs": catalog_list_after_queue_p95_ms,
            "catalogListAfterPosterQueueRequestCount": catalog_list_after_queue_ms.len(),
            "catalogListAfterPosterQueueHttpRequestProfile": catalog_list_after_queue_http_profile,
            "scanActiveAtApiStart": scan_active_at_api_start,
            "localPosterQueueCompleteMs": local_poster_queue_ms,
            "localPosterQueueFinalSnapshot":
                local_poster_queue_final_snapshot.report(file_count),
            "localPosterQueueDrainCondition": "all scan job local metadata batches are COMPLETED, every batch has images_completed_at, there are no pending/running/failed/cancelled batches, and the library has at least fileCount distinct LOCAL POSTER items",
            "onlineFillMissingJobCount": online_fill_missing_job_count,
            "processPeakRssBytes": process_peak_rss_bytes(),
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
            ..ScraperImagesResponse::default()
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
    Ok(get_request_elapsed_ms(
        &measure_get_request_samples(client, url, cookies, label).await?,
    ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GetRequestSample {
    request_id: String,
    path: String,
    elapsed_ms: u128,
    response_headers_us: u128,
    response_body_read_us: u128,
    response_body_bytes: usize,
    server_worker_wait_us: Option<u64>,
    server_handler_us: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IsolatedHttpSample {
    loadgen_process_id: u32,
    request_id: String,
    path: String,
    elapsed_us: u128,
    response_headers_us: u128,
    response_body_read_us: u128,
    response_body_bytes: usize,
    server_worker_wait_us: Option<u64>,
    server_handler_us: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct IsolatedHttpRequest {
    url: String,
    cookies: String,
    label: String,
    request_count: usize,
    probe_enabled: bool,
}

fn get_request_elapsed_ms(samples: &[GetRequestSample]) -> Vec<u128> {
    samples.iter().map(|sample| sample.elapsed_ms).collect()
}

fn performance_request_route_label(path: &str) -> &'static str {
    match path {
        "/api/v1/admin/libraries" => "admin_libraries",
        "/api/v1/home" => "home",
        "/api/v1/home/carousel" => "home_carousel",
        _ if path.starts_with("/api/v1/libraries/") && path.ends_with("/items") => "library_items",
        _ if path.starts_with("/api/v1/libraries/") && path.ends_with("/latest") => {
            "library_latest"
        }
        _ => "other",
    }
}

fn get_request_server_timing(sample: &GetRequestSample) -> Option<(u64, u64)> {
    if sample.request_id.is_empty() || sample.path.is_empty() {
        return None;
    }
    Some((sample.server_worker_wait_us?, sample.server_handler_us?))
}

fn get_request_profile(samples: &[GetRequestSample]) -> serde_json::Value {
    let elapsed_ms = samples
        .iter()
        .map(|sample| sample.elapsed_ms)
        .collect::<Vec<_>>();
    let response_headers_us = samples
        .iter()
        .map(|sample| sample.response_headers_us)
        .collect::<Vec<_>>();
    let response_body_read_us = samples
        .iter()
        .map(|sample| sample.response_body_read_us)
        .collect::<Vec<_>>();
    let response_body_bytes = samples
        .iter()
        .map(|sample| sample.response_body_bytes as u128)
        .collect::<Vec<_>>();
    let server_timing_samples = samples
        .iter()
        .filter_map(get_request_server_timing)
        .map(|sample| (u128::from(sample.0), u128::from(sample.1)))
        .collect::<Vec<_>>();
    let server_worker_wait_us = server_timing_samples
        .iter()
        .map(|sample| sample.0)
        .collect::<Vec<_>>();
    let server_handler_us = server_timing_samples
        .iter()
        .map(|sample| sample.1)
        .collect::<Vec<_>>();
    let server_timing_status = if server_timing_samples.is_empty() {
        "unavailable"
    } else if server_timing_samples.len() == samples.len() {
        "available"
    } else {
        "partial"
    };
    let worker_wait_p50_us =
        (!server_timing_samples.is_empty()).then(|| percentile(&server_worker_wait_us, 50));
    let worker_wait_p95_us =
        (!server_timing_samples.is_empty()).then(|| percentile(&server_worker_wait_us, 95));
    let handler_p50_us =
        (!server_timing_samples.is_empty()).then(|| percentile(&server_handler_us, 50));
    let handler_p95_us =
        (!server_timing_samples.is_empty()).then(|| percentile(&server_handler_us, 95));
    json!({
        "requestCount": samples.len(),
        "elapsedP50Ms": percentile(&elapsed_ms, 50),
        "elapsedP95Ms": percentile(&elapsed_ms, 95),
        "responseHeadersP50Us": percentile(&response_headers_us, 50),
        "responseHeadersP95Us": percentile(&response_headers_us, 95),
        "responseBodyReadP50Us": percentile(&response_body_read_us, 50),
        "responseBodyReadP95Us": percentile(&response_body_read_us, 95),
        "responseBodyBytesP50": percentile(&response_body_bytes, 50),
        "responseBodyBytesP95": percentile(&response_body_bytes, 95),
        "serverTiming": {
            "status": server_timing_status,
            "requestCount": samples.len(),
            "availableRequestCount": server_timing_samples.len(),
            "unavailableRequestCount": samples.len().saturating_sub(server_timing_samples.len()),
            "workerWaitP50Us": worker_wait_p50_us,
            "workerWaitP95Us": worker_wait_p95_us,
            "handlerP50Us": handler_p50_us,
            "handlerP95Us": handler_p95_us,
        },
        "requestSamples": samples.iter().map(|sample| json!({
            "routeLabel": performance_request_route_label(&sample.path),
            "elapsedMs": sample.elapsed_ms,
            "responseHeadersUs": sample.response_headers_us,
            "responseBodyReadUs": sample.response_body_read_us,
            "responseBodyBytes": sample.response_body_bytes,
            "serverTimingMatched": get_request_server_timing(sample).is_some(),
            "serverWorkerWaitUs": sample.server_worker_wait_us,
            "serverHandlerUs": sample.server_handler_us,
        })).collect::<Vec<_>>(),
    })
}

#[test]
fn get_request_profile_separates_headers_body_and_payload_size() {
    let samples = vec![
        GetRequestSample {
            request_id: "probe-1".to_owned(),
            path: "/api/v1/libraries/private-library-id/items".to_owned(),
            elapsed_ms: 11,
            response_headers_us: 8_100,
            response_body_read_us: 3_100,
            response_body_bytes: 100,
            server_worker_wait_us: Some(10),
            server_handler_us: Some(100),
        },
        GetRequestSample {
            request_id: "probe-2".to_owned(),
            path: "/api/v1/libraries/1/items".to_owned(),
            elapsed_ms: 22,
            response_headers_us: 15_100,
            response_body_read_us: 7_100,
            response_body_bytes: 200,
            server_worker_wait_us: Some(20),
            server_handler_us: Some(200),
        },
        GetRequestSample {
            request_id: "probe-3".to_owned(),
            path: "/api/v1/libraries/1/items".to_owned(),
            elapsed_ms: 33,
            response_headers_us: 24_100,
            response_body_read_us: 9_100,
            response_body_bytes: 300,
            server_worker_wait_us: Some(30),
            server_handler_us: Some(300),
        },
    ];

    let profile = get_request_profile(&samples);

    assert_eq!(profile["requestCount"], 3);
    assert_eq!(profile["elapsedP50Ms"], 22);
    assert_eq!(profile["elapsedP95Ms"], 33);
    assert_eq!(profile["responseHeadersP50Us"], 15_100);
    assert_eq!(profile["responseHeadersP95Us"], 24_100);
    assert_eq!(profile["responseBodyReadP50Us"], 7_100);
    assert_eq!(profile["responseBodyReadP95Us"], 9_100);
    assert_eq!(profile["responseBodyBytesP50"], 200);
    assert_eq!(profile["responseBodyBytesP95"], 300);
    assert_eq!(profile["requestSamples"][0]["routeLabel"], "library_items");
    assert_eq!(profile["requestSamples"][0]["serverTimingMatched"], true);
    assert_eq!(profile["requestSamples"][0]["serverWorkerWaitUs"], 10);
    assert_eq!(profile["requestSamples"][0]["serverHandlerUs"], 100);
    assert!(profile["requestSamples"][0].get("requestId").is_none());
    assert!(profile["requestSamples"][0].get("path").is_none());
    let serialized = serde_json::to_string(&profile).expect("profile should serialize");
    assert!(!serialized.contains("probe-1"));
    assert!(!serialized.contains("private-library-id"));
    assert_eq!(profile["requestSamples"].as_array().map(Vec::len), Some(3));
    assert_eq!(profile["serverTiming"]["status"], "available");
    assert_eq!(profile["serverTiming"]["availableRequestCount"], 3);
}

#[test]
fn get_request_profile_marks_missing_server_events_unavailable() {
    let profile = get_request_profile(&[GetRequestSample {
        request_id: "probe-4".to_owned(),
        path: "/api/v1/libraries/1/items".to_owned(),
        elapsed_ms: 9,
        response_headers_us: 7_000,
        response_body_read_us: 2_000,
        response_body_bytes: 64,
        server_worker_wait_us: None,
        server_handler_us: None,
    }]);

    assert_eq!(profile["requestCount"], 1);
    assert_eq!(profile["serverTiming"]["status"], "unavailable");
    assert_eq!(profile["serverTiming"]["availableRequestCount"], 0);
    assert_eq!(
        profile["serverTiming"]["workerWaitP95Us"],
        serde_json::Value::Null
    );
    assert_eq!(profile["requestSamples"][0]["serverTimingMatched"], false);
}

fn parse_performance_probe_timing(
    headers: &HeaderMap,
    expected_request_id: &str,
    expected_path: &str,
) -> Result<Option<(u64, u64)>, String> {
    fn header_value<'a>(
        headers: &'a HeaderMap,
        name: &'static str,
    ) -> Result<Option<&'a str>, String> {
        headers
            .get(name)
            .map(|value| {
                value
                    .to_str()
                    .map_err(|_| format!("performance probe response contains invalid {name}"))
            })
            .transpose()
    }

    let request_id = header_value(headers, "x-lux-perf-request-id")?;
    let path = header_value(headers, "x-lux-perf-path")?;
    let worker_wait_us = header_value(headers, "x-lux-perf-worker-wait-us")?;
    let handler_us = header_value(headers, "x-lux-perf-handler-us")?;
    if request_id.is_none() && path.is_none() && worker_wait_us.is_none() && handler_us.is_none() {
        return Ok(None);
    }
    if request_id != Some(expected_request_id) {
        return Err("performance probe response request ID does not match sample".to_owned());
    }
    if path != Some(expected_path) {
        return Err("performance probe response path does not match sample".to_owned());
    }
    let (Some(worker_wait_us), Some(handler_us)) = (worker_wait_us, handler_us) else {
        return Ok(None);
    };
    let worker_wait_us = worker_wait_us
        .parse::<u64>()
        .map_err(|_| "performance probe worker wait is not an integer".to_owned())?;
    let handler_us = handler_us
        .parse::<u64>()
        .map_err(|_| "performance probe handler time is not an integer".to_owned())?;
    Ok(Some((worker_wait_us, handler_us)))
}

#[test]
fn performance_probe_response_timing_requires_matching_request_id_and_path() {
    let mut headers = HeaderMap::new();
    headers.insert("x-lux-perf-request-id", HeaderValue::from_static("probe-7"));
    headers.insert(
        "x-lux-perf-path",
        HeaderValue::from_static("/api/v1/libraries/1/items"),
    );
    headers.insert("x-lux-perf-worker-wait-us", HeaderValue::from_static("17"));
    headers.insert("x-lux-perf-handler-us", HeaderValue::from_static("231"));

    assert_eq!(
        parse_performance_probe_timing(&headers, "probe-7", "/api/v1/libraries/1/items")
            .expect("matching server timing should parse"),
        Some((17, 231))
    );
    assert!(
        parse_performance_probe_timing(&headers, "probe-8", "/api/v1/libraries/1/items").is_err()
    );
    assert!(
        parse_performance_probe_timing(&headers, "probe-7", "/api/v1/libraries/2/items").is_err()
    );
    assert_eq!(
        parse_performance_probe_timing(&HeaderMap::new(), "probe-7", "/api/v1/libraries/1/items")
            .expect("missing diagnostics should be unavailable"),
        None
    );
    let mut incomplete_headers = headers.clone();
    incomplete_headers.remove("x-lux-perf-handler-us");
    assert_eq!(
        parse_performance_probe_timing(&incomplete_headers, "probe-7", "/api/v1/libraries/1/items")
            .expect("missing timing should be unavailable"),
        None
    );
}

#[test]
fn performance_probe_profile_counts_missing_events_as_unavailable() {
    let samples = vec![
        GetRequestSample {
            request_id: "probe-8".to_owned(),
            path: "/api/v1/libraries/1/items".to_owned(),
            elapsed_ms: 1,
            response_headers_us: 500,
            response_body_read_us: 500,
            response_body_bytes: 1,
            server_worker_wait_us: Some(10),
            server_handler_us: Some(20),
        },
        GetRequestSample {
            request_id: "probe-9".to_owned(),
            path: "/api/v1/libraries/1/items".to_owned(),
            elapsed_ms: 2,
            response_headers_us: 1_000,
            response_body_read_us: 1_000,
            response_body_bytes: 2,
            server_worker_wait_us: None,
            server_handler_us: None,
        },
    ];
    let profile = get_request_profile(&samples);

    assert_eq!(profile["requestCount"], 2);
    assert_eq!(profile["serverTiming"]["status"], "partial");
    assert_eq!(profile["serverTiming"]["availableRequestCount"], 1);
    assert_eq!(profile["serverTiming"]["unavailableRequestCount"], 1);
    assert_eq!(profile["requestSamples"].as_array().map(Vec::len), Some(2));
}

async fn measure_get_request_samples(
    client: &reqwest::Client,
    url: &str,
    cookies: &str,
    label: &str,
) -> Result<Vec<GetRequestSample>, Box<dyn std::error::Error>> {
    measure_get_request_samples_with_probe(client, url, cookies, label, FOREGROUND_REQUESTS, false)
        .await
}

async fn measure_get_request_samples_with_probe(
    client: &reqwest::Client,
    url: &str,
    cookies: &str,
    label: &str,
    request_count: usize,
    probe_enabled: bool,
) -> Result<Vec<GetRequestSample>, Box<dyn std::error::Error>> {
    if env::var("LUX_PERF_HTTP_CLIENT_MODE").as_deref() == Ok("process") {
        return Ok(measure_loadgen_in_child_process(
            url.to_owned(),
            cookies.to_owned(),
            label.to_owned(),
            request_count,
            probe_enabled,
        )
        .await?
        .into_iter()
        .map(|sample| GetRequestSample {
            request_id: sample.request_id,
            path: sample.path,
            elapsed_ms: sample.elapsed_us / 1_000,
            response_headers_us: sample.response_headers_us,
            response_body_read_us: sample.response_body_read_us,
            response_body_bytes: sample.response_body_bytes,
            server_worker_wait_us: sample.server_worker_wait_us,
            server_handler_us: sample.server_handler_us,
        })
        .collect());
    }

    let request_path = reqwest::Url::parse(url)?.path().to_owned();
    let mut requests = Vec::with_capacity(request_count);
    for _ in 0..request_count {
        let client = client.clone();
        let url = url.to_owned();
        let request_path = request_path.clone();
        let request_id = format!(
            "lux-perf-{}-{}",
            std::process::id(),
            NEXT_PERFORMANCE_PROBE_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
        );
        let cookies = cookies.to_owned();
        let label = label.to_owned();
        requests.push(tokio::spawn(async move {
            let started = Instant::now();
            let mut request = client.get(url).header(COOKIE, cookies);
            if probe_enabled {
                request = request
                    .header("x-request-id", request_id.as_str())
                    .header("x-lux-performance-probe", "v1");
            }
            let response = request.send().await.map_err(|error| error.to_string())?;
            let response_headers_us = started.elapsed().as_micros();
            let status = response.status();
            let response_headers = response.headers().clone();
            let response_body_started = Instant::now();
            let response_body = response.bytes().await.map_err(|error| error.to_string())?;
            let response_body_read_us = response_body_started.elapsed().as_micros();
            let elapsed_ms = started.elapsed().as_millis();
            if status != reqwest::StatusCode::OK {
                return Err(format!("{label} request returned {status}"));
            }
            let (server_worker_wait_us, server_handler_us) = if probe_enabled {
                parse_performance_probe_timing(&response_headers, &request_id, &request_path)
                    .map_err(|error| format!("{label} diagnostic event: {error}"))?
                    .map_or((None, None), |(worker_wait_us, handler_us)| {
                        (Some(worker_wait_us), Some(handler_us))
                    })
            } else {
                (None, None)
            };
            Ok::<GetRequestSample, String>(GetRequestSample {
                request_id,
                path: request_path,
                elapsed_ms,
                response_headers_us,
                response_body_read_us,
                response_body_bytes: response_body.len(),
                server_worker_wait_us,
                server_handler_us,
            })
        }));
    }
    let mut samples = Vec::with_capacity(FOREGROUND_REQUESTS);
    for request in requests {
        let result = request
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        samples.push(result.map_err(std::io::Error::other)?);
    }
    Ok(samples)
}

async fn measure_loadgen_in_child_process(
    url: String,
    cookies: String,
    label: String,
    request_count: usize,
    probe_enabled: bool,
) -> Result<Vec<IsolatedHttpSample>, Box<dyn std::error::Error>> {
    if !(1..=256).contains(&request_count) {
        return Err("isolated loadgen request count must be between 1 and 256".into());
    }
    let origin = isolated_loadgen_origin(&url)?;
    let input = IsolatedHttpRequest {
        url,
        cookies,
        label,
        request_count,
        probe_enabled,
    };
    let input = serde_json::to_string(&input)?;
    let executable = env::current_exe()?;
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        let children = ISOLATED_LOADGEN_CHILDREN.get_or_init(|| Mutex::new(BTreeMap::new()));
        let mut children = children
            .lock()
            .map_err(|_| std::io::Error::other("loadgen child registry was poisoned"))?;
        let exited = children
            .get_mut(&origin)
            .map(|child| child.child.try_wait().map(|status| status.is_some()))
            .transpose()?
            .unwrap_or(true);
        if exited {
            if let Some(mut previous_child) = children.remove(&origin) {
                let _ = previous_child.child.wait();
            }
            children.insert(origin.clone(), PersistentLoadgenChild::spawn(executable)?);
        }
        let result = children
            .get_mut(&origin)
            .ok_or_else(|| std::io::Error::other("loadgen child was not registered"))?
            .request(&input);
        if result.is_err()
            && let Some(mut failed_child) = children.remove(&origin)
        {
            let _ = failed_child.child.kill();
            let _ = failed_child.child.wait();
        }
        result
    })
    .await??;
    Ok(serde_json::from_str(&result)?)
}

async fn run_isolated_loadgen_batch(
    client: &reqwest::Client,
    input: IsolatedHttpRequest,
) -> Result<Vec<IsolatedHttpSample>, Box<dyn std::error::Error>> {
    if !(1..=256).contains(&input.request_count) {
        return Err("isolated loadgen request count must be between 1 and 256".into());
    }
    let parsed_url = reqwest::Url::parse(&input.url)?;
    let request_path = parsed_url.path().to_owned();
    let batch_id = NEXT_PERFORMANCE_PROBE_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let mut requests = Vec::with_capacity(input.request_count);
    for request_index in 0..input.request_count {
        let client = client.clone();
        let url = input.url.clone();
        let cookies = input.cookies.clone();
        let label = input.label.clone();
        let request_path = request_path.clone();
        let probe_enabled = input.probe_enabled;
        let request_id = format!(
            "lux-perf-loadgen-{}-{batch_id}-{request_index}",
            std::process::id()
        );
        requests.push(tokio::spawn(async move {
            let started = Instant::now();
            let mut request = client.get(url).header(COOKIE, cookies);
            if probe_enabled {
                request = request
                    .header("x-request-id", request_id.as_str())
                    .header("x-lux-performance-probe", "v1");
            }
            let response = request
                .send()
                .await
                .map_err(|_| format!("{label} request failed before response headers"))?;
            let response_headers_us = started.elapsed().as_micros();
            let status = response.status();
            let response_headers = response.headers().clone();
            let (server_worker_wait_us, server_handler_us) = parse_isolated_server_timing(
                &response_headers,
                &request_id,
                &request_path,
                probe_enabled,
            )?;
            let response_body_started = Instant::now();
            let response_body = response
                .bytes()
                .await
                .map_err(|_| format!("{label} response body read failed"))?;
            let response_body_read_us = response_body_started.elapsed().as_micros();
            let elapsed_us = started.elapsed().as_micros();
            if status != reqwest::StatusCode::OK {
                return Err(format!("{label} request returned {status}"));
            }
            Ok::<IsolatedHttpSample, String>(IsolatedHttpSample {
                loadgen_process_id: std::process::id(),
                request_id,
                path: request_path,
                elapsed_us,
                response_headers_us,
                response_body_read_us,
                response_body_bytes: response_body.len(),
                server_worker_wait_us,
                server_handler_us,
            })
        }));
    }
    let mut samples = Vec::with_capacity(input.request_count);
    for request in requests {
        samples.push(
            request
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))?
                .map_err(std::io::Error::other)?,
        );
    }
    Ok(samples)
}

fn parse_isolated_server_timing(
    headers: &reqwest::header::HeaderMap,
    expected_request_id: &str,
    expected_path: &str,
    probe_enabled: bool,
) -> Result<(Option<u64>, Option<u64>), String> {
    if !probe_enabled {
        return Ok((None, None));
    }
    let header = |name: &'static str| headers.get(name).and_then(|value| value.to_str().ok());
    let request_id = header("x-lux-perf-request-id");
    let path = header("x-lux-perf-path");
    let worker_wait = header("x-lux-perf-worker-wait-us");
    let handler = header("x-lux-perf-handler-us");
    if request_id.is_none() && path.is_none() && worker_wait.is_none() && handler.is_none() {
        return Ok((None, None));
    }
    if request_id != Some(expected_request_id) || path != Some(expected_path) {
        return Err("isolated loadgen received mismatched server probe metadata".to_owned());
    }
    match (worker_wait, handler) {
        (Some(worker_wait), Some(handler)) => Ok((
            Some(
                worker_wait
                    .parse()
                    .map_err(|_| "server worker wait was not an integer".to_owned())?,
            ),
            Some(
                handler
                    .parse()
                    .map_err(|_| "server handler time was not an integer".to_owned())?,
            ),
        )),
        _ => Ok((None, None)),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn performance_loadgen_child_process_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let observed_connections = Arc::new(Mutex::new(BTreeSet::new()));
    let handler_connections = observed_connections.clone();
    let app = Router::new().route(
        "/loadgen-probe",
        get(move |ConnectInfo(peer): ConnectInfo<SocketAddr>| {
            let handler_connections = handler_connections.clone();
            async move {
                if let Ok(mut connections) = handler_connections.lock() {
                    connections.insert(peer.port());
                }
                "loadgen-ok"
            }
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    let _loadgen_child_guard = IsolatedLoadgenChildGuard::new(&format!("http://{address}"))?;

    let local_client = reqwest::Client::builder().no_proxy().build()?;
    for _ in 0..2 {
        let response = local_client
            .get(format!("http://{address}/loadgen-probe"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let _ = response.bytes().await?;
    }
    let local_connection_count = observed_connections
        .lock()
        .map_err(|_| "connection observation lock was poisoned")?
        .len();
    assert_eq!(local_connection_count, 1, "fixture must support keep-alive");
    observed_connections
        .lock()
        .map_err(|_| "connection observation lock was poisoned")?
        .clear();

    let samples = measure_loadgen_in_child_process(
        format!("http://{address}/loadgen-probe"),
        "lux_session=synthetic".to_owned(),
        "isolated loadgen regression".to_owned(),
        4,
        false,
    )
    .await;
    let second_batch = measure_loadgen_in_child_process(
        format!("http://{address}/loadgen-probe"),
        "lux_session=synthetic".to_owned(),
        "isolated loadgen connection reuse regression".to_owned(),
        4,
        false,
    )
    .await;
    server.abort();
    let samples = samples?;
    let second_batch = second_batch?;

    assert_eq!(samples.len(), 4);
    assert_eq!(second_batch.len(), 4);
    assert!(
        samples
            .iter()
            .all(|sample| { sample.loadgen_process_id == second_batch[0].loadgen_process_id }),
        "sequential batches must share one client process"
    );
    assert_ne!(
        samples[0].request_id, second_batch[0].request_id,
        "batch IDs must advance within the persistent child process"
    );
    assert!(samples.iter().all(|sample| sample.path == "/loadgen-probe"));
    assert!(
        samples
            .iter()
            .all(|sample| sample.response_body_bytes == 10)
    );
    assert!(samples.iter().all(|sample| sample.elapsed_us > 0));
    let unique_connections = observed_connections
        .lock()
        .map_err(|_| "connection observation lock was poisoned")?
        .len();
    assert_eq!(
        unique_connections,
        4,
        "two sequential batches should reuse the same four HTTP connections; observed={:?}",
        observed_connections.lock().ok()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "invoked by the parent benchmark as an isolated HTTP load generator"]
async fn isolated_performance_loadgen_child() -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .pool_max_idle_per_host(256)
        .build()?;
    let stdin = std::io::stdin();
    let mut input_line = String::new();
    loop {
        input_line.clear();
        if stdin.read_line(&mut input_line)? == 0 {
            break;
        }
        let input: IsolatedHttpRequest = serde_json::from_str(&input_line)?;
        let samples = run_isolated_loadgen_batch(&client, input).await?;
        println!(
            "LUX_PERF_LOADGEN_RESULT:{}",
            serde_json::to_string(&samples)?
        );
        std::io::stdout().flush()?;
    }
    Ok(())
}

#[tokio::test]
async fn completed_scan_probe_signal_is_not_polled_twice() {
    let (local_poster_sender, local_poster_receiver) = tokio::sync::oneshot::channel();
    let (scan_completed_sender, scan_completed_receiver) = tokio::sync::oneshot::channel();
    assert!(
        local_poster_sender
            .send(("indexed-movie".to_owned(), 2))
            .is_ok()
    );
    assert!(scan_completed_sender.send(()).is_ok());

    let database_directory = tempfile::tempdir().expect("test database directory should exist");
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097"
            .parse()
            .expect("test HTTP address should be valid"),
        config_dir: database_directory.path().join("config"),
    })
    .await
    .expect("test database should initialize");

    let probe = probe_catalog_poster_until_scan_completes(
        reqwest::Client::new(),
        "http://127.0.0.1:1/catalog?itemType=MOVIE&sortBy=Name&sortOrder=Ascending&pageSize=100"
            .to_owned(),
        "http://127.0.0.1:1".to_owned(),
        String::new(),
        database,
        String::new(),
        "?",
        Instant::now(),
        local_poster_receiver,
        scan_completed_receiver,
    )
    .await
    .expect("the completed scan probe should finish without polling its signal twice");

    assert!(probe.poster_visible_ms.is_none());
    assert!(!probe.last_error.unwrap_or_default().is_empty());
}

async fn probe_catalog_poster_until_scan_completes(
    client: reqwest::Client,
    catalog_url: String,
    base_url: String,
    cookies: String,
    database: Database,
    library_id: String,
    library_id_placeholder: &str,
    scan_started: Instant,
    mut local_poster_indexed: tokio::sync::oneshot::Receiver<(String, i64)>,
    mut scan_completed: tokio::sync::oneshot::Receiver<()>,
) -> Result<CatalogPosterApiProbe, String> {
    let (poster_item_id, poster_page) = tokio::select! {
        biased;
        ready = &mut local_poster_indexed => ready.map_err(|_| "LUX-304 poster API probe was not notified about local poster indexing".to_owned())?,
        _ = &mut scan_completed => return Err("LUX-304 scan completed before a local poster row became available to probe".to_owned()),
    };
    let item_id_placeholder = if library_id_placeholder.starts_with('$') {
        "$2"
    } else {
        "?"
    };
    let catalog_position_query = format!(
        "WITH ranked_movies AS (
             SELECT item.id,
                    ROW_NUMBER() OVER (ORDER BY LOWER(item.title) ASC, item.id ASC) AS position
             FROM media_items item
             WHERE item.library_id = {library_id_placeholder}
               AND item.item_type = 'MOVIE'
               AND item.removed_at IS NULL
               AND item.merged_into_item_id IS NULL
               AND item.has_available_source = 1
         )
         SELECT position FROM ranked_movies WHERE id = {item_id_placeholder}"
    );

    let mut probe = CatalogPosterApiProbe {
        local_poster_index_observed: true,
        catalog_page: Some(poster_page),
        item_id: Some(poster_item_id.clone()),
        ..CatalogPosterApiProbe::default()
    };
    let mut scan_completed_observed = false;
    let deadline = Instant::now() + POSTER_API_PROBE_MAX_DURATION;
    for attempt in 0..POSTER_API_PROBE_MAX_REQUESTS {
        if Instant::now() >= deadline {
            break;
        }
        if attempt > 0 {
            tokio::select! {
                biased;
                _ = &mut scan_completed, if !scan_completed_observed => {
                    scan_completed_observed = true;
                },
                _ = tokio::time::sleep(deadline.saturating_duration_since(Instant::now())) => break,
                _ = tokio::time::sleep(POSTER_API_PROBE_INTERVAL) => {},
            }
        }

        probe.catalog_rank_lookup_count += 1;
        let current_position =
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(catalog_position_query.clone()))
                .bind(&library_id)
                .bind(&poster_item_id)
                .fetch_optional(database.pool())
                .await
                .map_err(|error| format!("LUX-304 catalog-rank lookup failed: {error}"))?;
        let catalog_page = current_position
            .and_then(catalog_poster_page_for_movie_position)
            .unwrap_or(poster_page);
        probe.catalog_page = Some(catalog_page);
        let request_catalog_url = format!("{catalog_url}&page={catalog_page}");
        probe.catalog_list_request_count += 1;
        let catalog_response = tokio::select! {
            biased;
            _ = &mut scan_completed, if !scan_completed_observed => {
                scan_completed_observed = true;
                None
            },
            response = client
                .get(&request_catalog_url)
                .header(COOKIE, &cookies)
                .timeout(POSTER_API_PROBE_REQUEST_TIMEOUT)
                .send() => Some(response),
        };
        let Some(catalog_response) = catalog_response else {
            continue;
        };
        let response = match catalog_response {
            Ok(response) if response.status() == reqwest::StatusCode::OK => response,
            Ok(response) => {
                probe.last_error = Some(format!("catalog endpoint returned {}", response.status()));
                continue;
            }
            Err(error) => {
                probe.last_error = Some(format!("catalog request failed: {error}"));
                continue;
            }
        };
        let catalog_body = tokio::select! {
            biased;
            _ = &mut scan_completed, if !scan_completed_observed => {
                scan_completed_observed = true;
                None
            },
            body = response.json::<serde_json::Value>() => Some(body),
        };
        let Some(catalog_body) = catalog_body else {
            continue;
        };
        let body = match catalog_body {
            Ok(body) => body,
            Err(error) => {
                probe.last_error = Some(format!("catalog response JSON was invalid: {error}"));
                continue;
            }
        };
        let target_item_present = catalog_page_contains_item(&body, &poster_item_id);
        if target_item_present && probe.catalog_item_visible_ms.is_none() {
            probe.catalog_item_visible_ms = Some(scan_started.elapsed().as_millis());
        }
        let Some((item_id, poster_tag)) = catalog_item_with_poster(&body, &poster_item_id) else {
            let visibility = if target_item_present {
                "lists the indexed item without imageTags.poster"
            } else {
                "does not list the indexed item"
            };
            let page_items = body.get("items").and_then(serde_json::Value::as_array);
            let page_item_count = page_items.map_or(0, Vec::len);
            let first_items = page_items
                .into_iter()
                .flat_map(|items| items.iter())
                .take(5)
                .filter_map(|item| {
                    let item_id = item.get("id")?.as_str()?;
                    let title = item
                        .get("title")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    Some(format!("{item_id}:{title}"))
                })
                .collect::<Vec<_>>();
            let total = body.get("total").and_then(serde_json::Value::as_i64);
            probe.last_error = Some(format!(
                "catalog page {catalog_page} {visibility}: {poster_item_id}, current_position={current_position:?}; total={total:?}, page_item_count={page_item_count}, first_items={first_items:?}"
            ));
            continue;
        };

        probe.item_id = Some(item_id.clone());
        probe.poster_tag = Some(poster_tag);
        if probe.poster_tag_visible_ms.is_none() {
            probe.poster_tag_visible_ms = Some(scan_started.elapsed().as_millis());
        }
        probe.poster_endpoint_request_count += 1;
        let poster_url = format!("{base_url}/api/v1/items/{item_id}/images/poster");
        let endpoint_started = Instant::now();
        let poster_response = tokio::select! {
            biased;
            _ = &mut scan_completed, if !scan_completed_observed => {
                scan_completed_observed = true;
                None
            },
            response = client
                .get(poster_url)
                .header(COOKIE, &cookies)
                .timeout(POSTER_API_PROBE_REQUEST_TIMEOUT)
                .send() => Some(response),
        };
        let Some(poster_response) = poster_response else {
            continue;
        };
        let response = match poster_response {
            Ok(response) if response.status() == reqwest::StatusCode::OK => response,
            Ok(response) => {
                probe.last_error = Some(format!(
                    "poster endpoint for item {item_id} returned {}",
                    response.status()
                ));
                continue;
            }
            Err(error) => {
                probe.last_error =
                    Some(format!("poster request for item {item_id} failed: {error}"));
                continue;
            }
        };
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let Some(content_type) = content_type else {
            probe.last_error = Some(format!(
                "poster endpoint for item {item_id} omitted content-type"
            ));
            continue;
        };
        if !content_type
            .split(';')
            .next()
            .map(str::trim)
            .is_some_and(|value| value.starts_with("image/"))
        {
            probe.last_error = Some(format!(
                "poster endpoint for item {item_id} returned non-image content-type {content_type}"
            ));
            continue;
        }
        let image_body = tokio::select! {
            biased;
            _ = &mut scan_completed, if !scan_completed_observed => {
                scan_completed_observed = true;
                None
            },
            body = response.bytes() => Some(body),
        };
        let Some(image_body) = image_body else {
            continue;
        };
        let image_bytes = match image_body {
            Ok(bytes) => bytes,
            Err(error) => {
                probe.last_error = Some(format!("poster response body read failed: {error}"));
                continue;
            }
        };
        if image_bytes.is_empty() || image::load_from_memory(&image_bytes).is_err() {
            probe.last_error = Some(format!(
                "poster endpoint for item {item_id} returned an empty or invalid image body"
            ));
            continue;
        }
        probe.poster_endpoint_latency_ms = Some(endpoint_started.elapsed().as_millis());
        probe.content_type = Some(content_type);
        probe.body_bytes = Some(image_bytes.len());
        probe.body_decoded_as_image = true;
        probe.poster_visible_ms = Some(scan_started.elapsed().as_millis());
        probe.last_error = None;
        return Ok(probe);
    }

    if !scan_completed_observed {
        let _ = scan_completed.await;
    }
    Ok(probe)
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
