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
const METADATA_BENCHMARK_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];
type ScanStageSample = (u64, u64, u64, u64);

#[derive(Default)]
struct QueryStatementCounts {
    statements: AtomicUsize,
    dml_statements: AtomicUsize,
    dml_summaries: Mutex<std::collections::HashMap<String, usize>>,
    unclassified_cte_summaries: Mutex<std::collections::HashMap<String, usize>>,
    manifest_application_ms: AtomicUsize,
    manifest_transaction_ms: AtomicUsize,
    manifest_apply_timing_batches: AtomicUsize,
    manifest_positive_commit_batches: AtomicUsize,
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
    if incoming_observation_cte || truncated_path_observation_cte || truncated_seen_paths_cte {
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

#[test]
fn query_counter_classifies_dml_statements_inside_common_table_expressions() {
    assert!(is_dml_statement_summary(
        "WITH sidecar_directories(directory) AS (VALUES (?)) INSERT INTO scan_job_targets"
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
        let summary = visitor
            .summary
            .unwrap_or_default()
            .trim()
            .to_ascii_uppercase();
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
    let sqlite_synchronous_level = if backend == "sqlite" {
        Some(
            sqlx::query_scalar::<_, i64>("PRAGMA synchronous")
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
        .create_library("LUX-270 Manifest Performance", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-utf8 fixture path")?,
        )
        .await?;
    let derived_index_triggers_disabled =
        backend == "sqlite" && env::var_os("LUX_PERF_DISABLE_DERIVED_INDEX_TRIGGERS").is_some();
    if derived_index_triggers_disabled {
        for trigger in [
            "media_items_search_ai",
            "media_items_search_au",
            "media_items_search_ad",
            "media_item_provider_ids_ai",
            "media_item_provider_ids_au",
        ] {
            let query = format!("DROP TRIGGER IF EXISTS {trigger}");
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
    let (raw_statement_count, dml_statement_count) = statement_counts.snapshot();
    let dml_summary_counts = statement_counts.dml_summary_snapshot();
    let unclassified_cte_summaries = statement_counts.unclassified_cte_summary_snapshot();
    let scan_statement_count = raw_statement_count.saturating_sub(postgres_lock_wait_samples);
    let scan_job_target_statement_count = dml_summary_counts
        .iter()
        .filter(|(_, summary)| summary.contains("INSERT INTO SCAN_JOB_TARGETS"))
        .map(|(count, _)| count)
        .sum::<usize>();
    let positive_commit_batch_count = statement_counts.manifest_positive_commit_batch_count();
    let max_positive_commit_batch_count =
        file_count.div_ceil(8_000) + directory_count.div_ceil(200) + 5;
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
    let target_materialization_started = Instant::now();
    jobs.materialize_manifest_postprocessing_targets(&job.id)
        .await?;
    let target_materialization_duration = target_materialization_started.elapsed();
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
    let foreground_ms = measure_get_requests(
        &client,
        &format!("{base_url}/api/v1/admin/libraries"),
        &cookies,
        "Manifest foreground",
    )
    .await?;
    let catalog_list_ms = measure_get_requests(
        &client,
        &format!(
            "{base_url}/api/v1/libraries/{}/items?page=1&pageSize=50",
            library.id
        ),
        &cookies,
        "Manifest catalog list",
    )
    .await?;
    let (rescan_processed, rescan_batch_durations) = rescan_handle
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
        .map_err(std::io::Error::other)?;
    let rescan_ms = rescan_started.elapsed().as_millis();
    let unchanged_rescan_stage_timings = statement_counts.scan_stage_values();
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
    assert_eq!(rescan_presence, (3, file_count as i64, 0));
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

    let report_sections = [
        json!({
            "commit": luxd::COMMIT,
            "architecture": std::env::consts::ARCH,
            "databaseBackend": backend,
            "derivedIndexTriggersDisabled": derived_index_triggers_disabled,
            "fileCount": file_count,
            "manifestIndexMs": manifest_index_ms,
            "manifestFilesProcessed": first_scan_processed,
            "manifestBatchCount": first_batch_durations.len(),
            "manifestPhaseMs": phase_durations,
            "manifestPhaseBatchCounts": phase_batch_counts,
            "manifestStageTimings": manifest_stage_timings,
            "manifestPositivePreparationMs": manifest_application_ms,
            "manifestPositiveCommitMs": manifest_transaction_ms,
            "manifestPositiveTimingEventCount": manifest_apply_timing_batches,
            "manifestPositiveCommitBatchCount": positive_commit_batch_count,
            "manifestPreparationConcurrency": manifest_preparation_concurrency,
            "manifestDirectoryReadConcurrency": manifest_directory_read_concurrency,
            "postprocessingTargetMaterializationMs": postprocessing_target_materialization_ms,
            "targetStageTimings": target_stage_timings,
            "postprocessingTargetSqlStatementCount": postprocessing_target_sql_count,
            "postprocessingTargetDmlStatementCount": postprocessing_target_dml_count,
            "postprocessingTargetCount": postprocessing_target_count,
            "batchP50Ms": batch_p50_ms,
            "batchP95Ms": batch_p95_ms,
        }),
        json!({
            "manifestSqlStatementCount": scan_statement_count,
            "manifestDmlStatementCount": dml_statement_count,
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
        }),
        json!({
            "foregroundDuringScan": scan_running_before_api,
            "foregroundRequestCount": FOREGROUND_REQUESTS,
            "foregroundP95Ms": foreground_p95_ms,
            "catalogListP95Ms": catalog_list_p95_ms,
            "unchangedRescanMs": rescan_ms,
            "unchangedRescanStageTimings": unchanged_rescan_stage_timings,
            "unchangedRescanBatchCount": rescan_batch_durations.len(),
            "unchangedRescanProcessed": rescan_processed,
            "manifestState": initial_manifest.0,
            "manifestObservedFiles": initial_manifest.1,
            "manifestAddedFiles": initial_manifest.3,
            "manifestPresence": manifest_presence,
            "sqlStatementCountNote": "SQLx statement events counted; PostgreSQL pg_stat_activity monitor SELECTs excluded. SQLite lock monitor acquires BEGIN IMMEDIATE every 100ms and commits immediately; its statements are included if surfaced by SQLx instrumentation. DML count classifies INSERT/UPDATE/DELETE/REPLACE/TRUNCATE summaries, including common-table-expression writes."
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
