use std::{
    collections::BTreeMap,
    env, fs,
    path::PathBuf,
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
        nfo::LocalNfoMetadataStore,
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
use reqwest::header::{COOKIE, SET_COOKIE};
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

#[derive(Clone, Copy)]
struct CatalogPageExpectation {
    fixture_file_count: usize,
    require_complete_fixture: bool,
    require_posters: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RequestTiming {
    start_offset_ns: u128,
    elapsed_ns: u128,
}

#[test]
fn process_peak_rss_is_reported_in_bytes() {
    assert!(process_peak_rss_bytes().is_some_and(|bytes| bytes > 0));
}

#[test]
fn performance_catalog_samples_reject_empty_and_foreign_fixture_pages() {
    let expectation = CatalogPageExpectation {
        fixture_file_count: 2,
        require_complete_fixture: false,
        require_posters: false,
    };

    assert!(validate_fixture_catalog_page(&json!({"items": [], "total": 0}), expectation).is_err());
    assert!(
        validate_fixture_catalog_page(
            &json!({
                "items": [{"itemType": "MOVIE", "title": "Unrelated Movie"}],
                "total": 1
            }),
            expectation
        )
        .is_err()
    );
    assert!(
        validate_fixture_catalog_page(
            &json!({
                "items": [{"itemType": "MOVIE", "title": "Fixture Movie 000000"}],
                "total": 3
            }),
            expectation
        )
        .is_err()
    );
}

#[test]
fn performance_catalog_drain_page_matches_fixture_and_has_posters() {
    let drained_page = json!({
        "items": [
            {"itemType": "MOVIE", "title": "Fixture Movie 000000", "imageTags": {"poster": "poster-0"}},
            {"itemType": "MOVIE", "title": "Fixture Movie 000001", "imageTags": {"poster": "poster-1"}}
        ],
        "total": 2
    });
    let expectation = CatalogPageExpectation {
        fixture_file_count: 2,
        require_complete_fixture: true,
        require_posters: true,
    };

    assert!(validate_fixture_catalog_page(&drained_page, expectation).is_ok());
    let missing_poster = json!({
        "items": [
            {"itemType": "MOVIE", "title": "Fixture Movie 000000", "imageTags": {"poster": "poster-0"}},
            {"itemType": "MOVIE", "title": "Fixture Movie 000001", "imageTags": {"poster": null}}
        ],
        "total": 2
    });
    assert!(validate_fixture_catalog_page(&missing_poster, expectation).is_err());
    let missing_fixture_item = json!({
        "items": [{"itemType": "MOVIE", "title": "Fixture Movie 000000", "imageTags": {"poster": "poster-0"}}],
        "total": 1
    });
    assert!(validate_fixture_catalog_page(&missing_fixture_item, expectation).is_err());
}

#[test]
fn scan_active_request_samples_must_fit_entirely_inside_the_window() {
    let requests = [
        RequestTiming {
            start_offset_ns: 99,
            elapsed_ns: 1,
        },
        RequestTiming {
            start_offset_ns: 100,
            elapsed_ns: 100,
        },
        RequestTiming {
            start_offset_ns: 150,
            elapsed_ns: 151,
        },
        RequestTiming {
            start_offset_ns: 200,
            elapsed_ns: 100,
        },
        RequestTiming {
            start_offset_ns: 250,
            elapsed_ns: 51,
        },
    ];

    assert_eq!(
        select_scan_active_requests(&requests, 100, 300),
        [requests[1], requests[3]]
    );
}

#[test]
fn scan_active_request_samples_are_unavailable_when_no_request_fits() {
    let requests = [RequestTiming {
        start_offset_ns: 50,
        elapsed_ns: 51,
    }];

    assert!(select_scan_active_requests(&requests, 100, 300).is_empty());
    assert_eq!(optional_percentile_ms(&[]), None);
}

#[test]
fn request_timing_report_keeps_slow_requests_started_during_scan() {
    let mut requests = vec![
        RequestTiming {
            start_offset_ns: 150_000_000,
            elapsed_ns: 10_000_000
        };
        FOREGROUND_REQUESTS
    ];
    for request in &mut requests[..3] {
        *request = RequestTiming {
            start_offset_ns: 100_000_000,
            elapsed_ns: 300_000_000,
        };
    }
    let report = request_timing_report(&requests, 100_000_000, 200_000_000);
    assert_eq!(report["startedDuringScanRequestCount"], 50);
    assert_eq!(report["crossingScanEndRequestCount"], 3);
    assert_eq!(report["startedDuringScanP95Ms"], 300);
    assert_eq!(report["scanActiveP95Ms"], serde_json::Value::Null);
    let incomplete = request_timing_report(&requests[..49], 100_000_000, 200_000_000);
    assert_eq!(
        incomplete["startedDuringScanP95Ms"],
        serde_json::Value::Null
    );
    requests[49].start_offset_ns = 200_000_000;
    let late = request_timing_report(&requests, 100_000_000, 200_000_000);
    assert_eq!(late["startedDuringScanP95Ms"], serde_json::Value::Null);
}

fn select_scan_active_requests(
    requests: &[RequestTiming],
    scan_start_ns: u128,
    scan_end_ns: u128,
) -> Vec<RequestTiming> {
    requests
        .iter()
        .copied()
        .filter(|request| {
            request.start_offset_ns >= scan_start_ns
                && request
                    .start_offset_ns
                    .checked_add(request.elapsed_ns)
                    .is_some_and(|end| end <= scan_end_ns)
        })
        .collect()
}

fn optional_percentile_ms(values: &[u128]) -> Option<u128> {
    (!values.is_empty()).then(|| percentile(values, 95))
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

async fn wait_for_fixture_catalog_page(
    client: &reqwest::Client,
    url: &str,
    cookies: &str,
    label: &str,
    expectation: CatalogPageExpectation,
) -> Result<(u128, serde_json::Value), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let started = Instant::now();
        let response = client.get(url).header(COOKIE, cookies).send().await?;
        let status = response.status();
        let body = response.bytes().await?;
        if status != reqwest::StatusCode::OK {
            return Err(format!("{label} request returned {status}").into());
        }
        let page: serde_json::Value = serde_json::from_slice(&body)?;
        let total = page["total"].as_u64().unwrap_or_default();
        let items_empty = page["items"].as_array().is_none_or(Vec::is_empty);
        if total > 0 && !items_empty {
            validate_fixture_catalog_page(&page, expectation).map_err(std::io::Error::other)?;
            return Ok((started.elapsed().as_millis(), page));
        }
        if Instant::now() >= deadline {
            return Err(format!("{label} never returned a visible fixture item").into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

const METADATA_BENCHMARK_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
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

// The baseline processes local metadata inline; the candidate drains an outbox.
// Keep their completion evidence explicit while sharing all HTTP sampling code.
enum BenchmarkPosterSnapshot {
    Detached(LocalPosterQueueSnapshot),
    Inline {
        total: i64,
        status: String,
        posters: i64,
    },
}

impl BenchmarkPosterSnapshot {
    fn is_drained(&self, required: usize) -> bool {
        match self {
            Self::Detached(snapshot) => snapshot.is_drained(required),
            Self::Inline {
                total,
                status,
                posters,
            } => status == "COMPLETED" && *total >= required as i64 && *posters >= required as i64,
        }
    }
    fn has_completed_batch_missing_image_marker(&self) -> bool {
        matches!(self, Self::Detached(snapshot) if snapshot.has_completed_batch_missing_image_marker())
    }
    fn is_pending_measurement_window(&self) -> bool {
        matches!(self, Self::Detached(snapshot) if snapshot.is_pending_measurement_window())
    }
    fn cancelled_batch_count(&self) -> i64 {
        match self {
            Self::Detached(snapshot) => snapshot.cancelled_batch_count,
            Self::Inline { status, .. } => i64::from(status == "CANCELLED" || status == "FAILED"),
        }
    }
    fn poster_item_count(&self) -> i64 {
        match self {
            Self::Detached(snapshot) => snapshot.poster_item_count,
            Self::Inline { posters, .. } => *posters,
        }
    }
    fn pending_measurement_unavailable_reason(&self) -> &'static str {
        match self {
            Self::Detached(snapshot) => snapshot.pending_measurement_unavailable_reason(),
            Self::Inline { .. } => "inline baseline has no detached image queue",
        }
    }
    fn report(&self, required: usize) -> serde_json::Value {
        match self {
            Self::Detached(snapshot) => snapshot.report(required),
            Self::Inline {
                total,
                status,
                posters,
            } => json!({
                "completionModel": "inline_metadata_baseline",
                "scanTotalCount": total, "scanStatus": status,
                "posterItemCount": posters, "requiredPosterItemCount": required,
                "drained": self.is_drained(required),
            }),
        }
    }
}

#[test]
fn inline_poster_drain_requires_scan_completion_and_all_fixture_posters() {
    assert!(
        BenchmarkPosterSnapshot::Inline {
            total: 100,
            status: "COMPLETED".into(),
            posters: 100
        }
        .is_drained(100)
    );
    for (status, posters) in [("RUNNING", 100), ("FAILED", 100), ("COMPLETED", 99)] {
        assert!(
            !BenchmarkPosterSnapshot::Inline {
                total: 100,
                status: status.into(),
                posters
            }
            .is_drained(100)
        );
    }
}

async fn load_benchmark_poster_snapshot(
    pool: &sqlx::AnyPool,
    job_id: &str,
    library_id: &str,
    job_placeholder: &str,
    library_placeholder: &str,
) -> Result<BenchmarkPosterSnapshot, sqlx::Error> {
    match env::var("LUX_PERF_POSTER_QUEUE_MODE").as_deref() {
        Err(_) | Ok("detached_batches") => Ok(BenchmarkPosterSnapshot::Detached(
            load_local_poster_queue_snapshot(
                pool,
                job_id,
                library_id,
                job_placeholder,
                library_placeholder,
            )
            .await?,
        )),
        Ok("inline_metadata_baseline") => {
            let library_placeholder = if job_placeholder == "$1" { "$2" } else { "?" };
            let query = format!("SELECT job.total_count, job.status,
                (SELECT COUNT(DISTINCT image.item_id) FROM item_images image
                 JOIN media_items item ON item.id = image.item_id
                 WHERE item.library_id = {library_placeholder} AND image.image_type = 'POSTER' AND image.source = 'LOCAL')
                FROM scan_jobs job WHERE job.id = {job_placeholder}");
            // SQLite binds the subquery before the WHERE clause; PostgreSQL
            // parameter numbering is independent of textual occurrence order.
            let (first, second) = if job_placeholder == "$1" {
                (job_id, library_id)
            } else {
                (library_id, job_id)
            };
            let (total, status, posters) = sqlx::query_as(sqlx::AssertSqlSafe(query))
                .bind(first)
                .bind(second)
                .fetch_one(pool)
                .await?;
            Ok(BenchmarkPosterSnapshot::Inline {
                total,
                status,
                posters,
            })
        }
        Ok(_) => Err(sqlx::Error::Protocol(
            "unsupported poster queue mode".into(),
        )),
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

async fn measure_catalog_get_requests_with_pool_pressure(
    client: &reqwest::Client,
    pool: sqlx::AnyPool,
    url: &str,
    cookies: &str,
    label: &str,
    expectation: CatalogPageExpectation,
) -> Result<(Vec<u128>, serde_json::Value), Box<dyn std::error::Error>> {
    let monitor = start_pool_pressure_monitor(pool);
    let request_result =
        measure_catalog_get_requests(client, url, cookies, label, expectation).await;
    let pressure = monitor.stop().await;
    Ok((request_result?, pressure))
}

async fn measure_get_request_checked(
    client: &reqwest::Client,
    url: &str,
    cookies: &str,
    label: &str,
    expectation: CatalogPageExpectation,
) -> Result<u128, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let response = client.get(url).header(COOKIE, cookies).send().await?;
    let status = response.status();
    let body = response.bytes().await?;
    if status != reqwest::StatusCode::OK {
        return Err(format!("{label} request returned {status}").into());
    }
    let elapsed_ms = started.elapsed().as_millis();
    let page: serde_json::Value = serde_json::from_slice(&body)?;
    validate_fixture_catalog_page(&page, expectation).map_err(std::io::Error::other)?;
    Ok(elapsed_ms)
}

async fn measure_catalog_get_request(
    client: &reqwest::Client,
    url: &str,
    cookies: &str,
    label: &str,
    expectation: CatalogPageExpectation,
) -> Result<u128, Box<dyn std::error::Error>> {
    measure_get_request_checked(client, url, cookies, label, expectation).await
}

async fn measure_catalog_get_request_with_pool_pressure(
    client: &reqwest::Client,
    pool: sqlx::AnyPool,
    url: &str,
    cookies: &str,
    label: &str,
    expectation: CatalogPageExpectation,
) -> Result<(u128, serde_json::Value), Box<dyn std::error::Error>> {
    let monitor = start_pool_pressure_monitor(pool);
    let request_result =
        measure_catalog_get_request(client, url, cookies, label, expectation).await;
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
    let catalog_page_expectation = CatalogPageExpectation {
        fixture_file_count: file_count,
        require_complete_fixture: false,
        require_posters: false,
    };
    let (catalog_list_first_request_ms, catalog_list_first_request_pool_pressure) =
        measure_catalog_get_request_with_pool_pressure(
            &client,
            database.pool().clone(),
            &catalog_page_url,
            &cookies,
            "Manifest first catalog page",
            catalog_page_expectation,
        )
        .await?;
    let catalog_list_first_request_sql_latency_window = statement_counts.take_query_latency_values(
        "catalog_list_first_request",
        true,
        "SQLx events observed during the single first catalog-page request; global instrumentation may include concurrent background SQL.",
    );
    let (catalog_list_warm_page_ms, catalog_list_warm_pool_pressure) =
        measure_catalog_get_requests_with_pool_pressure(
            &client,
            database.pool().clone(),
            &catalog_page_url,
            &cookies,
            "Manifest warmed catalog page",
            catalog_page_expectation,
        )
        .await?;
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
                return Ok((
                    processed,
                    rescan_batch_durations,
                    rescan_started.elapsed().as_nanos(),
                ));
            }
        }
    });
    tokio::task::yield_now().await;
    let scan_running_before_api = !rescan_handle.is_finished();
    let foreground_monitor = start_pool_pressure_monitor(database.pool().clone());
    let catalog_monitor = start_pool_pressure_monitor(database.pool().clone());
    let admin_url = format!("{base_url}/api/v1/admin/libraries");
    let (foreground_timings, catalog_timings) = tokio::try_join!(
        measure_timed_get_requests(
            &client,
            &admin_url,
            &cookies,
            "Manifest foreground",
            None,
            rescan_started
        ),
        measure_timed_get_requests(
            &client,
            &catalog_page_url,
            &cookies,
            "Manifest catalog list",
            Some(catalog_page_expectation),
            rescan_started
        ),
    )?;
    let foreground_pool_pressure = foreground_monitor.stop().await;
    let catalog_list_pool_pressure = catalog_monitor.stop().await;
    let foreground_ms: Vec<_> = foreground_timings
        .iter()
        .map(|sample| sample.elapsed_ns / 1_000_000)
        .collect();
    let catalog_list_ms: Vec<_> = catalog_timings
        .iter()
        .map(|sample| sample.elapsed_ns / 1_000_000)
        .collect();
    let (rescan_processed, rescan_batch_durations, rescan_end_ns) = rescan_handle
        .await
        .map_err(std::io::Error::other)?
        .map_err(std::io::Error::other)?;
    let rescan_ms = rescan_end_ns / 1_000_000;
    let rescan_and_request_window_ms = rescan_started.elapsed().as_millis();
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
            "foregroundRequestTiming": request_timing_report(&foreground_timings, 0, rescan_end_ns),
            "catalogListRequestTiming": request_timing_report(&catalog_timings, 0, rescan_end_ns),
            "rescanAndRequestWindowMs": rescan_and_request_window_ms,
            "foregroundRequestCount": FOREGROUND_REQUESTS,
            "foregroundP95Ms": foreground_p95_ms,
            "foregroundPoolPressure": foreground_pool_pressure,
            "catalogListP95Ms": catalog_list_p95_ms,
            "catalogListPoolPressure": catalog_list_pool_pressure,
            "catalogListFirstRequestMs": catalog_list_first_request_ms,
            "catalogListFirstRequestPoolPressure": catalog_list_first_request_pool_pressure,
            "catalogListFirstRequestSqlLatencyWindow":
                catalog_list_first_request_sql_latency_window,
            "catalogListWarmP50Ms": percentile(&catalog_list_warm_page_ms, 50),
            "catalogListWarmP95Ms": percentile(&catalog_list_warm_page_ms, 95),
            "catalogListWarmRequestCount": catalog_list_warm_page_ms.len(),
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
    let jobs = ScanJobService::new(database.clone())
        .with_nfo_store(LocalNfoMetadataStore::new(database.clone()));
    let scan_job = jobs.create_movie_scan_job(library.id).await?;
    let scan_started = Instant::now();
    let scan_worker = jobs.clone();
    let scan_job_id = scan_job.id.clone();
    let scan_handle = tokio::spawn(async move {
        scan_worker
            .run_to_completion(&scan_job_id, 500, None)
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(scan_started.elapsed().as_nanos())
    });
    tokio::task::yield_now().await;
    // The direct worker does not invalidate AppState's catalog cache. Wait for
    // a committed fixture item before the first request so an empty page is
    // never cached for the scan-active sample.
    let catalog_list_url = format!(
        "{base_url}/api/v1/libraries/{}/items?page=1&pageSize=50",
        library.id
    );
    // Catalog cache keys include the page limit, so each phase gets a fresh
    // cache entry while sampling the same library and first page.
    let image_pending_catalog_list_url = format!(
        "{base_url}/api/v1/libraries/{}/items?page=1&pageSize=51",
        library.id
    );
    let drained_catalog_list_url = format!(
        "{base_url}/api/v1/libraries/{}/items?page=1&pageSize=52",
        library.id
    );
    let catalog_page_expectation = CatalogPageExpectation {
        fixture_file_count: file_count,
        require_complete_fixture: false,
        require_posters: false,
    };
    let first_poster_observer_database = database.clone();
    let first_poster_observer_library_id = library.id.to_string();
    let first_poster_observer_started = scan_started;
    let first_poster_indexed_handle = tokio::spawn(async move {
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            if sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(format!(
                "SELECT image.item_id FROM item_images image
                 JOIN media_items item ON item.id = image.item_id
                 WHERE item.library_id = {library_id_placeholder} AND image.image_type = 'POSTER'
                   AND image.source = 'LOCAL' LIMIT 1"
            )))
            .bind(&first_poster_observer_library_id)
            .fetch_optional(first_poster_observer_database.pool())
            .await?
            .is_some()
            {
                return Ok::<_, sqlx::Error>(first_poster_observer_started.elapsed().as_millis());
            }
            if Instant::now() >= deadline {
                return Err(sqlx::Error::Protocol(
                    "LUX-304 first poster visibility timed out".to_owned(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    let first_item_deadline = Instant::now() + Duration::from_secs(600);
    let first_item_visible_ms = loop {
        if sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(format!(
            "SELECT id FROM media_items
             WHERE library_id = {library_id_placeholder} AND item_type = 'MOVIE' LIMIT 1"
        )))
        .bind(library.id.to_string())
        .fetch_optional(database.pool())
        .await?
        .is_some()
        {
            break scan_started.elapsed().as_millis();
        }
        if Instant::now() >= first_item_deadline {
            return Err("LUX-304 first item visibility timed out".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let page_ready_deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM media_items WHERE library_id = {library_id_placeholder} AND item_type = 'MOVIE'"
        ))).bind(library.id.to_string()).fetch_one(database.pool()).await?;
        if count >= 50 {
            break;
        }
        if Instant::now() >= page_ready_deadline {
            return Err("catalog page readiness timed out".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let scan_running_at_api_start = !scan_handle.is_finished();
    if !scan_running_at_api_start {
        return Err("LUX-304 scan completed before the scan-active catalog sample began".into());
    }
    let (first_catalog_request_ms, first_catalog_page) = wait_for_fixture_catalog_page(
        &client,
        &catalog_list_url,
        &cookies,
        "LUX-304 first visible catalog page",
        catalog_page_expectation,
    )
    .await?;
    let first_catalog_visible_total = first_catalog_page["total"].as_u64().unwrap_or_default();
    assert_eq!(
        first_catalog_page["items"].as_array().map(Vec::len),
        Some(50)
    );
    let catalog_timings = measure_timed_get_requests(
        &client,
        &catalog_list_url,
        &cookies,
        "LUX-304 progressive catalog list",
        Some(catalog_page_expectation),
        scan_started,
    )
    .await?;
    let catalog_list_ms: Vec<_> = catalog_timings
        .iter()
        .map(|sample| sample.elapsed_ns / 1_000_000)
        .collect();
    let scan_end_ns = scan_handle
        .await
        .map_err(std::io::Error::other)?
        .map_err(std::io::Error::other)?;
    let scan_job_completion_ms = scan_end_ns / 1_000_000;
    let catalog_timing_report = request_timing_report(&catalog_timings, 0, scan_end_ns);
    let scan_finished_queue_snapshot = load_benchmark_poster_snapshot(
        database.pool(),
        &scan_job.id,
        &library.id.to_string(),
        job_id_placeholder,
        queue_library_id_placeholder,
    )
    .await?;
    let first_poster_indexed_ms = first_poster_indexed_handle
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))??;
    let catalog_list_p95_ms = catalog_timing_report["scanActiveP95Ms"].as_u64();

    let require_detached_poster_queue = env::var("LUX_PERF_REQUIRE_DETACHED_POSTER_QUEUE")
        .map(|value| value != "0")
        .unwrap_or(true);
    let scan_finished_image_pending = scan_finished_queue_snapshot.is_pending_measurement_window();
    let catalog_list_after_scan_image_pending_ms = if scan_finished_image_pending {
        Some(
            measure_catalog_get_requests(
                &client,
                &image_pending_catalog_list_url,
                &cookies,
                "LUX-304 catalog list after scan while images are pending",
                catalog_page_expectation,
            )
            .await?,
        )
    } else {
        None
    };
    let scan_finished_queue_after_api_snapshot = load_benchmark_poster_snapshot(
        database.pool(),
        &scan_job.id,
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
        let snapshot = load_benchmark_poster_snapshot(
            database.pool(),
            &scan_job.id,
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
        if snapshot.cancelled_batch_count() > 0 {
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
    let drained_catalog_expectation = CatalogPageExpectation {
        fixture_file_count: file_count,
        require_complete_fixture: true,
        require_posters: true,
    };
    let catalog_list_after_queue_ms = measure_catalog_get_requests(
        &client,
        &drained_catalog_list_url,
        &cookies,
        "LUX-304 catalog list after local posters",
        drained_catalog_expectation,
    )
    .await?;
    let catalog_list_after_queue_p95_ms = percentile(&catalog_list_after_queue_ms, 95);
    let online_fill_missing_job_count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT COUNT(*) FROM metadata_reidentify_jobs
         WHERE library_id = {library_id_placeholder} AND mode = 'FILL_MISSING'"
    )))
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    assert_eq!(online_fill_missing_job_count, 0);
    assert!(scan_running_at_api_start);
    assert!(first_item_visible_ms <= scan_job_completion_ms);
    assert_eq!(
        local_poster_queue_final_snapshot.poster_item_count(),
        i64::try_from(file_count).unwrap_or(i64::MAX),
        "the drained poster count must exactly match the synthetic fixture"
    );
    assert!(first_catalog_visible_total > 0);
    assert!(
        first_poster_indexed_ms <= scan_job_completion_ms,
        "first local poster was indexed at {first_poster_indexed_ms} ms, after scan completion at {scan_job_completion_ms} ms"
    );
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
            "posterQueueMode": env::var("LUX_PERF_POSTER_QUEUE_MODE").unwrap_or_else(|_| "detached_batches".into()),
            "architecture": std::env::consts::ARCH,
            "databaseBackend": backend,
            "fileCount": file_count,
            "directoryCount": directory_count,
            "scanJobCompletionMs": scan_job_completion_ms,
            "firstItemVisibleMs": first_item_visible_ms,
            "firstCatalogRequestMs": first_catalog_request_ms,
            "firstCatalogVisibleTotal": first_catalog_visible_total,
            "firstPosterIndexedMs": first_poster_indexed_ms,
            "catalogListP95DuringScanMs": catalog_list_p95_ms,
            "catalogListP95RequestsStartedDuringScanMs": catalog_timing_report["startedDuringScanP95Ms"],
            "catalogListRequestTiming": catalog_timing_report,
            "catalogPageReadinessRule": "at least 50 committed movies before first HTTP page; pageSize=50 on both revisions",
            "firstCatalogVisibleItemCount": first_catalog_page["items"].as_array().map(Vec::len),
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
            "scanRunningAtApiStart": scan_running_at_api_start,
            "localPosterQueueCompleteMs": local_poster_queue_ms,
            "localPosterQueueFinalSnapshot":
                local_poster_queue_final_snapshot.report(file_count),
            "localPosterQueueDrainCondition": "detached: every batch COMPLETED with images_completed_at and full poster count; inline baseline: scan COMPLETED and full poster count",
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
    measure_get_requests_checked(client, url, cookies, label, None).await
}

async fn measure_catalog_get_requests(
    client: &reqwest::Client,
    url: &str,
    cookies: &str,
    label: &str,
    expectation: CatalogPageExpectation,
) -> Result<Vec<u128>, Box<dyn std::error::Error>> {
    measure_get_requests_checked(client, url, cookies, label, Some(expectation)).await
}

async fn measure_get_requests_checked(
    client: &reqwest::Client,
    url: &str,
    cookies: &str,
    label: &str,
    expectation: Option<CatalogPageExpectation>,
) -> Result<Vec<u128>, Box<dyn std::error::Error>> {
    Ok(
        measure_timed_get_requests(client, url, cookies, label, expectation, Instant::now())
            .await?
            .iter()
            .map(|request| request.elapsed_ns / 1_000_000)
            .collect(),
    )
}

async fn measure_timed_get_requests(
    client: &reqwest::Client,
    url: &str,
    cookies: &str,
    label: &str,
    expectation: Option<CatalogPageExpectation>,
    epoch: Instant,
) -> Result<Vec<RequestTiming>, Box<dyn std::error::Error>> {
    let mut requests = Vec::with_capacity(FOREGROUND_REQUESTS);
    for _ in 0..FOREGROUND_REQUESTS {
        let client = client.clone();
        let url = url.to_owned();
        let cookies = cookies.to_owned();
        requests.push(tokio::spawn(async move {
            let started = Instant::now();
            let response = client
                .get(url)
                .header(COOKIE, cookies)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status();
            let body = response.bytes().await.map_err(|error| error.to_string())?;
            let timing = RequestTiming {
                start_offset_ns: started.duration_since(epoch).as_nanos(),
                elapsed_ns: started.elapsed().as_nanos(),
            };
            Ok::<_, String>((timing, status, body))
        }));
    }
    let mut responses = Vec::with_capacity(FOREGROUND_REQUESTS);
    for request in requests {
        responses.push(
            request
                .await
                .map_err(std::io::Error::other)?
                .map_err(std::io::Error::other)?,
        );
    }
    let label = label.to_owned();
    // All timed requests have finished before parsing any response. JSON work
    // also stays off the Tokio workers shared with the service under test.
    Ok(tokio::task::spawn_blocking(move || {
        let mut timings = Vec::with_capacity(responses.len());
        for (timing, status, body) in responses {
            if status != reqwest::StatusCode::OK {
                return Err(format!("{label} request returned {status}"));
            }
            if let Some(expectation) = expectation {
                let page = serde_json::from_slice(&body)
                    .map_err(|error| format!("{label} returned invalid JSON: {error}"))?;
                validate_fixture_catalog_page(&page, expectation)?;
            }
            timings.push(timing);
        }
        Ok::<_, String>(timings)
    })
    .await
    .map_err(std::io::Error::other)?
    .map_err(std::io::Error::other)?)
}

fn request_timing_report(
    requests: &[RequestTiming],
    scan_start_ns: u128,
    scan_end_ns: u128,
) -> serde_json::Value {
    let active = select_scan_active_requests(requests, scan_start_ns, scan_end_ns);
    let started: Vec<_> = requests
        .iter()
        .filter(|request| {
            request.start_offset_ns >= scan_start_ns && request.start_offset_ns < scan_end_ns
        })
        .collect();
    let crossing = started
        .iter()
        .filter(|request| {
            request
                .start_offset_ns
                .checked_add(request.elapsed_ns)
                .is_none_or(|end| end > scan_end_ns)
        })
        .count();
    let durations: Vec<_> = started
        .iter()
        .map(|request| request.elapsed_ns / 1_000_000)
        .collect();
    let complete_start_cohort =
        requests.len() == FOREGROUND_REQUESTS && started.len() == FOREGROUND_REQUESTS;
    let complete_overlap = complete_start_cohort && active.len() == FOREGROUND_REQUESTS;
    json!({
        "scanStartOffsetNs": scan_start_ns,
        "scanEndOffsetNs": scan_end_ns,
        "expectedRequestCount": FOREGROUND_REQUESTS,
        "requestCount": requests.len(),
        "startedDuringScanRequestCount": started.len(),
        "crossingScanEndRequestCount": crossing,
        "scanActiveRequestCount": active.len(),
        "startedDuringScanP95Ms": complete_start_cohort.then(|| percentile(&durations, 95)),
        "scanActiveP95Ms": complete_overlap.then(|| percentile(&durations, 95)),
        "sampleStatus": if complete_start_cohort { "available" } else { "unavailable" },
        "wholeWindowSampleStatus": if complete_overlap { "available" } else { "unavailable" },
        "selectionRule": "all 50 predeclared requests start in [scanStart, scanEnd); full body-receipt latency includes crossings; no response-duration selection",
        "wholeWindowRule": "all 50 requests start and finish inside scan window; otherwise p95 is unavailable",
        "requests": requests.iter().enumerate().map(|(index, request)| json!({
            "index": index,
            "startOffsetNs": request.start_offset_ns,
            "elapsedNs": request.elapsed_ns,
            "endOffsetNs": request.start_offset_ns + request.elapsed_ns,
        })).collect::<Vec<_>>(),
    })
}

fn validate_fixture_catalog_page(
    page: &serde_json::Value,
    expectation: CatalogPageExpectation,
) -> Result<(), String> {
    let total = page["total"]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| "catalog total is missing or invalid".to_owned())?;
    if total == 0 {
        return Err("catalog total is zero".to_owned());
    }
    if total > expectation.fixture_file_count {
        return Err(format!(
            "catalog total {total} exceeds fixture size {}",
            expectation.fixture_file_count
        ));
    }
    if expectation.require_complete_fixture && total != expectation.fixture_file_count {
        return Err(format!(
            "drained catalog total {total} does not match fixture size {}",
            expectation.fixture_file_count
        ));
    }
    let items = page["items"]
        .as_array()
        .filter(|items| !items.is_empty())
        .ok_or_else(|| "catalog items page is empty or missing".to_owned())?;
    if items.len() > total {
        return Err(format!(
            "catalog page has {} items but total is {total}",
            items.len()
        ));
    }
    for item in items {
        if item["itemType"] != "MOVIE"
            || !item["title"]
                .as_str()
                .is_some_and(|title| title.starts_with("Fixture Movie "))
        {
            return Err("catalog page contains an item outside the movie fixture".to_owned());
        }
        if expectation.require_posters
            && !item["imageTags"]["poster"]
                .as_str()
                .is_some_and(|tag| !tag.is_empty())
        {
            return Err("drained catalog page contains a movie without a poster tag".to_owned());
        }
    }
    Ok(())
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
