//! Scheduled rating sync for the `org.lux.trakt-ratings` plugin.
//!
//! A run walks one library, asks the plugin for the provider rating of every item that already has
//! a sidecar NFO, and rewrites only the rating fields of those NFOs through
//! [`NfoWriteService::write_item_ratings`]. Because the write marks the file as Lux-owned, the
//! file watcher deliberately does not rescan it, so the run finishes by refreshing the directories
//! it changed.
//!
//! The run is a plain in-process background task: there is no job row, no progress endpoint and no
//! restart recovery yet (see `docs/decisions/053-scheduled-ratings-sync.md`). The only durable
//! effect is the sidecar content, and a rerun recomputes it from scratch.
//!
//! Design rules that the code below keeps:
//!
//! - nothing is requested for an item without a writable NFO, and an item is never looked up by
//!   title alone;
//! - the plugin response is untrusted and validated field by field before any write;
//! - a batch that cannot be delivered retries twice and then stops the run, while per-item errors
//!   only affect that item, except for credential errors which stop the run;
//! - everything that reaches the logs is a count, a library id, a plugin id or an item id.

mod output;
mod query;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fmt,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};
use uuid::Uuid;

use self::{
    output::{
        ItemFailure, MatchOutcome, classify_match, index_by_input, is_trusted_match,
        parse_batch_response, trakt_id_of,
    },
    query::{
        RatingsQuery, SeriesLookup, episode_query, movie_query, parse_provider_ids, show_query,
    },
};
use crate::{
    application::{
        nfo::{NfoWriteService, RatingsNfoPatch, RatingsWriteOutcome},
        plugins::PluginService,
        scanner::{ScanJobError, ScanJobService},
    },
    domain::ids::LibraryId,
    storage::{Database, StorageError, StoredLibraryRoot, StoredRatingsSyncItem},
};

/// Plugin method that resolves provider ratings for a batch of lookups.
pub const RATINGS_BATCH_METHOD: &str = "ratings.batch";

/// Lookups per plugin call. The plugin allows 200, but it answers them serially with a fixed
/// provider delay, so a smaller batch keeps one timeout from covering several minutes of work.
const BATCH_SIZE: usize = 50;
/// Items read from storage per page.
const ITEM_PAGE_SIZE: i64 = 500;
/// Directories per `start_local_metadata_refresh` call (the scanner's own limit).
const MAX_REFRESH_DIRECTORIES: usize = 2_000;
/// Flush the pending refresh work once this many changed directories accumulated.
const REFRESH_FLUSH_THRESHOLD: usize = 500;
/// How long shutdown waits for a run to notice the stop signal.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);

/// Deadlines and backoff for a run.
///
/// A production run uses the defaults; tests shorten them so the retry and rate-limit paths do not
/// spend real minutes waiting.
#[derive(Clone, Debug)]
pub struct RatingsSyncTiming {
    /// Deadline for one `ratings.batch` call.
    pub batch_timeout: Duration,
    /// Delays before the first and second retry of a failed batch call.
    pub batch_backoff: [Duration; 2],
    /// Wait after a batch before retrying its rate-limited or transient items.
    pub rate_limit_delay: Duration,
}

impl Default for RatingsSyncTiming {
    fn default() -> Self {
        Self {
            batch_timeout: Duration::from_secs(120),
            batch_backoff: [Duration::from_secs(5), Duration::from_secs(30)],
            rate_limit_delay: Duration::from_secs(60),
        }
    }
}

/// The handle a caller gets right after a run was accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RatingsSyncRun {
    pub id: String,
    pub library_id: String,
    pub plugin_id: String,
}

/// Why a rating sync could not be started.
#[derive(Debug)]
pub enum RatingsSyncError {
    InvalidLibraryId,
    LibraryNotFound,
    LibraryDisabled,
    /// The plugin is missing, disabled or unknown.
    PluginUnavailable,
    /// The plugin is installed but its configuration cannot drive a sync (`clientId` missing).
    PluginNotConfigured,
    /// A sync for this library is already running in this process.
    AlreadyActive,
    /// The provider rejected the credentials, so every further request would fail too.
    Unauthorized,
    /// A batch call failed even after both retries.
    BatchFailed,
    /// The plugin answered with a payload that does not match the protocol.
    InvalidResponse,
    Cancelled,
    Storage(StorageError),
}

impl fmt::Display for RatingsSyncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLibraryId => formatter.write_str("ratings sync library id is invalid"),
            Self::LibraryNotFound => formatter.write_str("ratings sync library does not exist"),
            Self::LibraryDisabled => formatter.write_str("ratings sync library is disabled"),
            Self::PluginUnavailable => formatter.write_str("ratings plugin is not available"),
            Self::PluginNotConfigured => {
                formatter.write_str("ratings plugin configuration is incomplete")
            }
            Self::AlreadyActive => formatter.write_str("a ratings sync is already running"),
            Self::Unauthorized => formatter.write_str("ratings provider rejected the credentials"),
            Self::BatchFailed => formatter.write_str("ratings provider batch request failed"),
            Self::InvalidResponse => formatter.write_str("ratings provider response is invalid"),
            Self::Cancelled => formatter.write_str("ratings sync was cancelled"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RatingsSyncError {}

impl From<StorageError> for RatingsSyncError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

/// Runs one library at a time and keeps at most one run per library.
#[derive(Clone)]
pub struct RatingsSyncService {
    database: Database,
    plugins: PluginService,
    nfo: NfoWriteService,
    scan_jobs: ScanJobService,
    /// Libraries with a run in flight. A `std` mutex is enough: the set is only touched for the
    /// length of an insert or remove, and the drop guard cannot await.
    active: Arc<StdMutex<HashSet<String>>>,
    shutdown: watch::Sender<bool>,
    tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    timing: RatingsSyncTiming,
}

/// Removes the in-flight marker even when the run returns early, panics or is dropped.
struct ActiveRun {
    active: Arc<StdMutex<HashSet<String>>>,
    library_id: String,
}

impl Drop for ActiveRun {
    fn drop(&mut self) {
        let mut active = lock_active(&self.active);
        active.remove(&self.library_id);
    }
}

fn lock_active(active: &StdMutex<HashSet<String>>) -> std::sync::MutexGuard<'_, HashSet<String>> {
    // A panic while holding the set cannot leave it inconsistent (one insert or remove), so the
    // poisoned value is still usable and no production path panics here on purpose.
    active
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl RatingsSyncService {
    pub fn new(
        database: Database,
        plugins: PluginService,
        nfo: NfoWriteService,
        scan_jobs: ScanJobService,
    ) -> Self {
        let (shutdown, _receiver) = watch::channel(false);
        Self {
            database,
            plugins,
            nfo,
            scan_jobs,
            active: Arc::new(StdMutex::new(HashSet::new())),
            shutdown,
            tasks: Arc::new(Mutex::new(Vec::new())),
            timing: RatingsSyncTiming::default(),
        }
    }

    /// Overrides the deadlines and backoff (tests only).
    #[doc(hidden)]
    pub fn with_timing(mut self, timing: RatingsSyncTiming) -> Self {
        self.timing = timing;
        self
    }

    /// Validates the preconditions and spawns the run, returning its id immediately.
    pub async fn start(
        &self,
        library_id: &str,
        plugin_id: &str,
    ) -> Result<RatingsSyncRun, RatingsSyncError> {
        let plugin_id = plugin_id.trim();
        if plugin_id.is_empty() {
            return Err(RatingsSyncError::PluginUnavailable);
        }
        let library_id = library_id
            .trim()
            .parse::<LibraryId>()
            .map_err(|_| RatingsSyncError::InvalidLibraryId)?
            .to_string();
        let library = self
            .database
            .find_library(&library_id)
            .await?
            .ok_or(RatingsSyncError::LibraryNotFound)?;
        if !library.is_enabled {
            return Err(RatingsSyncError::LibraryDisabled);
        }
        if !self
            .plugins
            .plugin_installed_and_enabled(plugin_id)
            .await
            .map_err(|_| RatingsSyncError::PluginUnavailable)?
        {
            return Err(RatingsSyncError::PluginUnavailable);
        }
        let values = self
            .plugins
            .plugin_config_values(plugin_id)
            .await
            .map_err(|_| RatingsSyncError::PluginNotConfigured)?;
        let client_id_configured = values
            .get("clientId")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|value| !value.is_empty());
        if !client_id_configured {
            return Err(RatingsSyncError::PluginNotConfigured);
        }
        let roots = self.database.list_library_roots(&library_id).await?;
        if roots.is_empty() {
            return Err(RatingsSyncError::LibraryNotFound);
        }
        {
            let mut active = lock_active(&self.active);
            if !active.insert(library_id.clone()) {
                return Err(RatingsSyncError::AlreadyActive);
            }
        }
        let run_id = Uuid::now_v7().to_string();
        let guard = ActiveRun {
            active: Arc::clone(&self.active),
            library_id: library_id.clone(),
        };
        let service = self.clone();
        let task_run_id = run_id.clone();
        let task_library_id = library_id.clone();
        let task_plugin_id = plugin_id.to_owned();
        let mut shutdown = self.shutdown.subscribe();
        let handle = tokio::spawn(async move {
            let _guard = guard;
            service
                .run(
                    task_run_id,
                    task_library_id,
                    task_plugin_id,
                    roots,
                    &mut shutdown,
                )
                .await;
        });
        let mut tasks = self.tasks.lock().await;
        tasks.retain(|handle| !handle.is_finished());
        tasks.push(handle);
        Ok(RatingsSyncRun {
            id: run_id,
            library_id,
            plugin_id: plugin_id.to_owned(),
        })
    }

    /// Waits until no run is in flight (tests use this instead of sleeping).
    #[doc(hidden)]
    pub async fn wait_for_idle(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        loop {
            if lock_active(&self.active).is_empty() {
                return true;
            }
            if started.elapsed() > timeout {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Signals every run to stop and waits a bounded time for them.
    pub async fn shutdown(&self) {
        self.shutdown.send_replace(true);
        let tasks = {
            let mut tasks = self.tasks.lock().await;
            std::mem::take(&mut *tasks)
        };
        // One deadline for every run: a run blocked in a 120 s plugin call must not stretch the
        // shutdown by that much per library.
        let deadline = Instant::now() + SHUTDOWN_WAIT;
        for task in tasks {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                tracing::warn!("ratings sync task did not stop before shutdown");
                continue;
            }
            match tokio::time::timeout(remaining, task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "ratings sync task stopped unexpectedly");
                }
                Err(_) => {
                    tracing::warn!("ratings sync task did not stop before shutdown");
                }
            }
        }
    }

    async fn run(
        &self,
        run_id: String,
        library_id: String,
        plugin_id: String,
        roots: Vec<StoredLibraryRoot>,
        shutdown: &mut watch::Receiver<bool>,
    ) {
        let started = Instant::now();
        let mut state = SyncState::new(roots).await;
        let outcome = self
            .sync_library(&library_id, &plugin_id, &mut state, shutdown)
            .await;
        // NFOs written before a stop must still be picked up: the internal-write marker suppresses
        // the watcher, so without this refresh the catalog keeps the old rating.
        self.flush_refresh(&library_id, &mut state).await;
        let elapsed = started.elapsed();
        let counters = &state.counters;
        match outcome {
            Ok(()) => tracing::info!(
                run_id = %run_id,
                library_id = %library_id,
                plugin_id = %plugin_id,
                elapsed_ms = elapsed.as_millis(),
                scanned = counters.scanned,
                queried = counters.queried,
                written = counters.written,
                unchanged = counters.unchanged,
                skipped_no_nfo = counters.skipped_no_nfo,
                skipped_no_id = counters.skipped_no_id,
                skipped_not_found = counters.skipped_not_found,
                skipped_not_confident = counters.skipped_not_confident,
                skipped_ambiguous = counters.skipped_ambiguous,
                skipped_invalid = counters.skipped_invalid,
                failed = counters.failed,
                refreshed_directories = counters.refreshed_directories,
                not_refreshable = counters.not_refreshable,
                "ratings sync finished"
            ),
            Err(RatingsSyncError::Cancelled) => tracing::info!(
                run_id = %run_id,
                library_id = %library_id,
                plugin_id = %plugin_id,
                elapsed_ms = elapsed.as_millis(),
                scanned = counters.scanned,
                queried = counters.queried,
                written = counters.written,
                unchanged = counters.unchanged,
                skipped_no_nfo = counters.skipped_no_nfo,
                skipped_no_id = counters.skipped_no_id,
                skipped_not_found = counters.skipped_not_found,
                skipped_not_confident = counters.skipped_not_confident,
                skipped_ambiguous = counters.skipped_ambiguous,
                skipped_invalid = counters.skipped_invalid,
                failed = counters.failed,
                refreshed_directories = counters.refreshed_directories,
                not_refreshable = counters.not_refreshable,
                "ratings sync stopped before shutdown"
            ),
            Err(error) => tracing::error!(
                run_id = %run_id,
                library_id = %library_id,
                plugin_id = %plugin_id,
                elapsed_ms = elapsed.as_millis(),
                scanned = counters.scanned,
                queried = counters.queried,
                written = counters.written,
                unchanged = counters.unchanged,
                skipped_no_nfo = counters.skipped_no_nfo,
                skipped_no_id = counters.skipped_no_id,
                skipped_not_found = counters.skipped_not_found,
                skipped_not_confident = counters.skipped_not_confident,
                skipped_ambiguous = counters.skipped_ambiguous,
                skipped_invalid = counters.skipped_invalid,
                failed = counters.failed,
                refreshed_directories = counters.refreshed_directories,
                not_refreshable = counters.not_refreshable,
                %error,
                "ratings sync failed"
            ),
        }
    }

    async fn sync_library(
        &self,
        library_id: &str,
        plugin_id: &str,
        state: &mut SyncState,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<(), RatingsSyncError> {
        // Series go first: their episodes need the Trakt id their own lookup returned.
        for pass in [SyncPass::Series, SyncPass::Movie, SyncPass::Episode] {
            self.sync_pass(pass, library_id, plugin_id, state, shutdown)
                .await?;
        }
        Ok(())
    }

    async fn sync_pass(
        &self,
        pass: SyncPass,
        library_id: &str,
        plugin_id: &str,
        state: &mut SyncState,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<(), RatingsSyncError> {
        let mut after_id: Option<String> = None;
        let mut retry = VecDeque::new();
        loop {
            if *shutdown.borrow() {
                return Err(RatingsSyncError::Cancelled);
            }
            if !self.library_is_enabled(library_id).await? {
                return Err(RatingsSyncError::LibraryNotFound);
            }
            let page = self
                .database
                .list_ratings_sync_items(
                    library_id,
                    pass.item_type(),
                    after_id.as_deref(),
                    ITEM_PAGE_SIZE,
                )
                .await?;
            if page.is_empty() {
                break;
            }
            after_id = page.last().map(|item| item.id.clone());
            let pending = self.prepare_page(pass, &page, state).await;
            self.send_batches(plugin_id, pending, state, shutdown, &mut retry, true)
                .await?;
            self.flush_if_due(library_id, state).await;
        }
        if !retry.is_empty() {
            if sleep_or_shutdown(shutdown, self.timing.rate_limit_delay).await {
                return Err(RatingsSyncError::Cancelled);
            }
            let mut ignored = VecDeque::new();
            self.send_batches(plugin_id, retry, state, shutdown, &mut ignored, false)
                .await?;
        }
        Ok(())
    }

    async fn library_is_enabled(&self, library_id: &str) -> Result<bool, RatingsSyncError> {
        Ok(self
            .database
            .find_library(library_id)
            .await?
            .is_some_and(|library| library.is_enabled))
    }

    /// Turns one page into lookups, skipping everything the run must not ask about.
    async fn prepare_page(
        &self,
        pass: SyncPass,
        page: &[StoredRatingsSyncItem],
        state: &mut SyncState,
    ) -> VecDeque<PendingItem> {
        let mut pending = VecDeque::with_capacity(page.len());
        for item in page {
            state.counters.scanned += 1;
            let ids = parse_provider_ids(item.provider_ids_json.as_deref());
            if pass == SyncPass::Series {
                // Remember the identity even when the series itself is skipped: its episodes may
                // still be written through a different NFO.
                state
                    .series
                    .insert(item.id.clone(), SeriesLookup::from_provider_ids(&ids));
            }
            match self.nfo.item_has_nfo(&item.id).await {
                Ok(true) => {}
                Ok(false) => {
                    state.counters.skipped_no_nfo += 1;
                    continue;
                }
                Err(error) => {
                    state.counters.failed += 1;
                    tracing::warn!(item_id = %item.id, %error, "ratings sync could not inspect an item");
                    continue;
                }
            }
            let query = match pass {
                SyncPass::Movie => movie_query(&item.title, &ids, item.production_year),
                SyncPass::Series => show_query(&item.title, &ids, item.production_year),
                SyncPass::Episode => {
                    let series = item.series_id.as_deref().or(item.parent_id.as_deref());
                    match series.and_then(|id| state.series.get(id)) {
                        Some(series) => {
                            episode_query(series, item.season_number, item.episode_number)
                        }
                        None => None,
                    }
                }
            };
            match query {
                Some(query) => pending.push_back(PendingItem {
                    item_id: item.id.clone(),
                    query,
                }),
                None => state.counters.skipped_no_id += 1,
            }
        }
        pending
    }

    async fn send_batches(
        &self,
        plugin_id: &str,
        mut queue: VecDeque<PendingItem>,
        state: &mut SyncState,
        shutdown: &mut watch::Receiver<bool>,
        retry: &mut VecDeque<PendingItem>,
        allow_retry: bool,
    ) -> Result<(), RatingsSyncError> {
        while !queue.is_empty() {
            if *shutdown.borrow() {
                return Err(RatingsSyncError::Cancelled);
            }
            let mut chunk = VecDeque::new();
            for _ in 0..BATCH_SIZE {
                match queue.pop_front() {
                    Some(pending) => chunk.push_back(pending),
                    None => break,
                }
            }
            self.send_batch(plugin_id, chunk, state, shutdown, retry, allow_retry)
                .await?;
        }
        Ok(())
    }

    /// Sends one batch, writes what it returned and records what has to be tried again.
    async fn send_batch(
        &self,
        plugin_id: &str,
        chunk: VecDeque<PendingItem>,
        state: &mut SyncState,
        shutdown: &mut watch::Receiver<bool>,
        retry: &mut VecDeque<PendingItem>,
        allow_retry: bool,
    ) -> Result<(), RatingsSyncError> {
        let entries = deduplicate(chunk);
        let items = entries
            .iter()
            .map(|entry| entry.query.to_json())
            .collect::<Vec<_>>();
        let keys = items.iter().map(output::canonical_json).collect::<Vec<_>>();
        state.counters.queried += entries.len() as u64;
        let params = json!({ "items": items });
        let response = self.call_batch(plugin_id, params, shutdown).await?;
        let Some(results) = parse_batch_response(&response, entries.len()) else {
            return Err(RatingsSyncError::InvalidResponse);
        };
        let positions = index_by_input(&keys);
        let mut handled = vec![false; entries.len()];
        for result in results {
            let key = output::canonical_json(&result.input);
            let Some(position) = positions
                .get(key.as_str())
                .and_then(|positions| positions.first())
                .copied()
            else {
                continue;
            };
            if handled[position] {
                continue;
            }
            handled[position] = true;
            let entry = &entries[position];
            match result.outcome {
                Ok(value) => {
                    if entry.store_series_id {
                        state.remember_series_id(&entry.item_ids, &value);
                    }
                    match classify_match(&value) {
                        MatchOutcome::Patch(patch) => {
                            for item_id in &entry.item_ids {
                                self.apply_patch(item_id, &patch, state).await;
                            }
                        }
                        MatchOutcome::NotConfident => {
                            state.counters.skipped_not_confident += entry.item_ids.len() as u64;
                        }
                        MatchOutcome::Invalid => {
                            state.counters.skipped_invalid += entry.item_ids.len() as u64;
                        }
                    }
                }
                Err(failure) => {
                    let count = entry.item_ids.len() as u64;
                    match failure {
                        ItemFailure::NotFound => state.counters.skipped_not_found += count,
                        ItemFailure::RateLimited | ItemFailure::Upstream if allow_retry => {
                            for item_id in &entry.item_ids {
                                retry.push_back(PendingItem {
                                    item_id: item_id.clone(),
                                    query: entry.query.clone(),
                                });
                            }
                        }
                        ItemFailure::RateLimited | ItemFailure::Upstream => {
                            state.counters.failed += count;
                        }
                        ItemFailure::Fatal => return Err(RatingsSyncError::Unauthorized),
                        ItemFailure::Invalid | ItemFailure::Unknown => {
                            state.counters.failed += count
                        }
                    }
                }
            }
        }
        for (position, entry) in entries.iter().enumerate() {
            if handled[position] {
                continue;
            }
            state.counters.failed += entry.item_ids.len() as u64;
            tracing::warn!(
                item_id = %entry.item_ids.first().map(String::as_str).unwrap_or_default(),
                "ratings sync got no result for an item"
            );
        }
        Ok(())
    }
}

/// Collapses identical lookups inside one batch so the provider is asked once.
fn deduplicate(chunk: VecDeque<PendingItem>) -> Vec<BatchEntry> {
    let mut entries: Vec<BatchEntry> = Vec::with_capacity(chunk.len());
    let mut positions = HashMap::<String, usize>::new();
    for pending in chunk {
        let key = output::canonical_json(&pending.query.to_json());
        if let Some(position) = positions.get(&key) {
            entries[*position].item_ids.push(pending.item_id);
            continue;
        }
        positions.insert(key, entries.len());
        entries.push(BatchEntry {
            store_series_id: matches!(
                pending.query,
                RatingsQuery::ShowById { .. } | RatingsQuery::ShowByTitle { .. }
            ),
            query: pending.query,
            item_ids: vec![pending.item_id],
        });
    }
    entries
}

impl RatingsSyncService {
    async fn call_batch(
        &self,
        plugin_id: &str,
        params: Value,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<Value, RatingsSyncError> {
        let mut attempt = 0;
        loop {
            match self
                .plugins
                .call_with_timeout(
                    plugin_id,
                    RATINGS_BATCH_METHOD,
                    params.clone(),
                    self.timing.batch_timeout,
                )
                .await
            {
                Ok(value) => return Ok(value),
                Err(_) => {
                    if attempt >= self.timing.batch_backoff.len() {
                        return Err(RatingsSyncError::BatchFailed);
                    }
                    let delay = self.timing.batch_backoff[attempt];
                    attempt += 1;
                    tracing::warn!(
                        plugin_id,
                        attempt,
                        "ratings batch request failed; retrying after the backoff"
                    );
                    if sleep_or_shutdown(shutdown, delay).await {
                        return Err(RatingsSyncError::Cancelled);
                    }
                }
            }
        }
    }

    async fn apply_patch(&self, item_id: &str, patch: &RatingsNfoPatch, state: &mut SyncState) {
        match self.nfo.write_item_ratings(item_id, patch).await {
            Ok(RatingsWriteOutcome::Written { path }) => {
                state.counters.written += 1;
                state.record_written(&path);
            }
            Ok(RatingsWriteOutcome::Unchanged) => state.counters.unchanged += 1,
            Ok(RatingsWriteOutcome::SkippedNoNfo) => state.counters.skipped_no_nfo += 1,
            Ok(RatingsWriteOutcome::SkippedAmbiguous) => state.counters.skipped_ambiguous += 1,
            Err(error) => {
                // One sidecar that cannot be written (concurrent edit, symlink, permissions) is a
                // failed item, not a reason to abandon the rest of the library.
                state.counters.failed += 1;
                tracing::warn!(item_id, %error, "ratings sync could not write a sidecar");
            }
        }
    }

    async fn flush_if_due(&self, library_id: &str, state: &mut SyncState) {
        if state.pending_directories() >= REFRESH_FLUSH_THRESHOLD {
            self.flush_refresh(library_id, state).await;
        }
    }

    /// Refreshes every directory whose sidecar changed, grouped by library root.
    async fn flush_refresh(&self, library_id: &str, state: &mut SyncState) {
        let pending = std::mem::take(&mut state.changed_directories);
        if pending.is_empty() {
            return;
        }
        let Ok(library_id) = library_id.parse::<LibraryId>() else {
            tracing::warn!("ratings sync could not refresh a library with an invalid id");
            return;
        };
        for (root_id, directories) in pending {
            let directories = directories.into_iter().collect::<Vec<_>>();
            for chunk in directories.chunks(MAX_REFRESH_DIRECTORIES) {
                match self
                    .scan_jobs
                    .start_local_metadata_refresh(library_id, Some(&root_id), chunk)
                    .await
                {
                    Ok(accepted) => {
                        state.counters.refreshed_directories += accepted.directories as u64;
                    }
                    // An empty selection means the directories hold no indexed media file, which
                    // is the normal outcome when only sidecars changed.
                    Err(ScanJobError::NoChanges) => {}
                    Err(error) => {
                        tracing::warn!(
                            library_id = %library_id,
                            root_id = %root_id,
                            %error,
                            "ratings sync could not refresh written sidecars"
                        );
                    }
                }
            }
        }
    }
}

/// One item waiting for a lookup.
#[derive(Clone, Debug)]
struct PendingItem {
    item_id: String,
    query: RatingsQuery,
}

/// One unique lookup inside a batch, together with every item that shares it.
#[derive(Clone, Debug)]
struct BatchEntry {
    query: RatingsQuery,
    item_ids: Vec<String>,
    store_series_id: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SyncPass {
    Series,
    Movie,
    Episode,
}

impl SyncPass {
    const fn item_type(self) -> &'static str {
        match self {
            Self::Series => "SERIES",
            Self::Movie => "MOVIE",
            Self::Episode => "EPISODE",
        }
    }
}

#[derive(Debug, Default)]
struct RatingsSyncCounters {
    scanned: u64,
    queried: u64,
    written: u64,
    unchanged: u64,
    skipped_no_nfo: u64,
    skipped_no_id: u64,
    skipped_not_found: u64,
    skipped_not_confident: u64,
    skipped_ambiguous: u64,
    skipped_invalid: u64,
    failed: u64,
    refreshed_directories: u64,
    not_refreshable: u64,
}

struct SyncState {
    /// Library roots as `(id, canonical path)`.
    roots: Vec<(String, PathBuf)>,
    /// Series lookups keyed by the series item id, filled during the series pass.
    series: HashMap<String, SeriesLookup>,
    /// Changed NFO directories keyed by the library root they belong to.
    changed_directories: BTreeMap<String, BTreeSet<String>>,
    counters: RatingsSyncCounters,
}

impl SyncState {
    async fn new(roots: Vec<StoredLibraryRoot>) -> Self {
        let mut resolved = Vec::with_capacity(roots.len());
        for root in roots {
            let path = match tokio::fs::canonicalize(&root.canonical_path).await {
                Ok(path) => path,
                Err(_) => PathBuf::from(&root.canonical_path),
            };
            resolved.push((root.id, path));
        }
        Self {
            roots: resolved,
            series: HashMap::new(),
            changed_directories: BTreeMap::new(),
            counters: RatingsSyncCounters::default(),
        }
    }

    fn pending_directories(&self) -> usize {
        self.changed_directories
            .values()
            .map(BTreeSet::len)
            .sum::<usize>()
    }

    /// Keeps the numeric Trakt id a trustworthy series lookup returned for later episode lookups.
    fn remember_series_id(&mut self, series_ids: &[String], result: &Value) {
        if !is_trusted_match(result) {
            return;
        }
        let Some(trakt_id) = trakt_id_of(result) else {
            return;
        };
        for series_id in series_ids {
            if let Some(series) = self.series.get_mut(series_id) {
                series.remember_trakt_id(trakt_id);
            }
        }
    }

    /// Records the directory of a written sidecar under the library root that contains it.
    fn record_written(&mut self, nfo_path: &Path) {
        let mut best: Option<(&str, &Path)> = None;
        for (root_id, root) in &self.roots {
            if !nfo_path.starts_with(root) {
                continue;
            }
            if best.is_none_or(|(_, best_root)| {
                root.components().count() > best_root.components().count()
            }) {
                best = Some((root_id.as_str(), root.as_path()));
            }
        }
        let Some((root_id, root)) = best else {
            self.counters.not_refreshable += 1;
            return;
        };
        match relative_directory(root, nfo_path) {
            Some(directory) => {
                self.changed_directories
                    .entry(root_id.to_owned())
                    .or_default()
                    .insert(directory);
            }
            // A sidecar directly in the library root has no relative directory to pass, and the
            // scanner rejects an empty path, so it cannot be refreshed this way.
            None => self.counters.not_refreshable += 1,
        }
    }
}

/// The directory of `nfo_path` relative to `root`, with `/` separators.
///
/// `None` means the sidecar sits directly in the root (the local-metadata refresh has no way to
/// name the root itself).
fn relative_directory(root: &Path, nfo_path: &Path) -> Option<String> {
    let parent = nfo_path.parent()?;
    let relative = parent.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

async fn sleep_or_shutdown(shutdown: &mut watch::Receiver<bool>, duration: Duration) -> bool {
    if *shutdown.borrow() {
        return true;
    }
    tokio::select! {
        () = tokio::time::sleep(duration) => *shutdown.borrow(),
        result = shutdown.changed() => result.is_err() || *shutdown.borrow(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn relative_directory_stays_inside_the_library_root() {
        let root = Path::new("/media/movies");
        assert_eq!(
            relative_directory(root, Path::new("/media/movies/Dune (2021)/movie.nfo")),
            Some("Dune (2021)".to_owned())
        );
        assert_eq!(
            relative_directory(
                root,
                Path::new("/media/movies/tv/Show/Season 01/episode.nfo")
            ),
            Some("tv/Show/Season 01".to_owned())
        );
        assert_eq!(
            relative_directory(root, Path::new("/media/movies/movie.nfo")),
            None,
            "the root itself has no relative path the scanner accepts"
        );
        assert_eq!(
            relative_directory(root, Path::new("/media/other/movie.nfo")),
            None
        );
    }

    #[test]
    fn written_directories_are_grouped_per_root_and_deduplicated() {
        let mut state = SyncState {
            roots: vec![
                ("root-a".to_owned(), PathBuf::from("/media/a")),
                ("root-b".to_owned(), PathBuf::from("/media/a/nested")),
            ],
            series: HashMap::new(),
            changed_directories: BTreeMap::new(),
            counters: RatingsSyncCounters::default(),
        };

        state.record_written(Path::new("/media/a/nested/Show/tvshow.nfo"));
        state.record_written(Path::new("/media/a/nested/Show/tvshow.nfo"));
        state.record_written(Path::new("/media/a/Movie/movie.nfo"));
        state.record_written(Path::new("/media/a/movie.nfo"));
        state.record_written(Path::new("/elsewhere/movie.nfo"));

        let root_b = state
            .changed_directories
            .get("root-b")
            .expect("the deepest root wins");
        assert_eq!(
            root_b.iter().cloned().collect::<Vec<_>>(),
            vec!["Show".to_owned()]
        );
        let root_a = state.changed_directories.get("root-a").expect("root a");
        assert_eq!(
            root_a.iter().cloned().collect::<Vec<_>>(),
            vec!["Movie".to_owned()]
        );
        assert_eq!(state.counters.not_refreshable, 2);
    }

    #[test]
    fn error_messages_never_carry_configuration_values() {
        for error in [
            RatingsSyncError::InvalidLibraryId,
            RatingsSyncError::LibraryNotFound,
            RatingsSyncError::LibraryDisabled,
            RatingsSyncError::PluginUnavailable,
            RatingsSyncError::PluginNotConfigured,
            RatingsSyncError::AlreadyActive,
            RatingsSyncError::Unauthorized,
            RatingsSyncError::BatchFailed,
            RatingsSyncError::InvalidResponse,
            RatingsSyncError::Cancelled,
        ] {
            let message = error.to_string();
            assert!(!message.contains("http"), "{message}");
            assert!(!message.is_empty());
        }
    }
}
