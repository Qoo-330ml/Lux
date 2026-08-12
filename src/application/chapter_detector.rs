use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    task::JoinSet,
    time::timeout,
};
use uuid::Uuid;

use crate::{
    application::{
        plugin_protocol::{
            CHAPTER_FINGERPRINT_POINT_DURATION_TICKS, CHAPTER_FINGERPRINT_SAMPLE_RATE,
            ChapterDetectMarkerType, ChapterDetectRpcMarker, ChapterDetectRpcRequest,
            ChapterFingerprintRpcEpisode, ChapterLookupRpcEpisode, ChapterLookupRpcRequest,
        },
        plugins::{PluginService, PluginServiceError},
    },
    domain::ids::LibraryId,
    storage::{
        Database, NewChapterDetectionJob, NewChapterDetectionJobItem, NewMediaChapterMarker,
        StoredChapterDetectionItem, StoredChapterDetectionJob,
    },
};

pub const DEFAULT_CHAPTER_DETECTOR_PLUGIN_ID: &str = "org.lux.intro-outro-detector";
const SOURCE_PAGE_SIZE: i64 = 500;
const MAX_EPISODES_PER_RPC: usize = 64;
const MAX_REMOTE_EPISODES_PER_RPC: usize = 24;
const MAX_CONCURRENCY: i64 = 16;
const MIN_WINDOW_SECONDS: i64 = 15;
const MAX_INTRO_WINDOW_SECONDS: i64 = 300;
const MAX_CREDITS_WINDOW_SECONDS: i64 = 600;
const MIN_MATCH_DURATION_TICKS: i64 = 10_000_000;
const MAX_FINGERPRINT_BYTES: usize = 384 * 1024;
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(90);
const FFMPEG_BINARY: &str = "ffmpeg";
const JOB_ERROR: &str = "one or more chapter detection sources failed";
const NO_FINGERPRINT: &[u8] = &[];
const LOCAL_MIN_EPISODES_PER_SEASON: usize = 3;
const REMOTE_MIN_EPISODES_PER_SEASON: usize = 1;
const FOUND_REFRESH_INTERVAL_SECONDS: i64 = 30 * 24 * 60 * 60;
const NOT_FOUND_RETRY_INTERVAL_SECONDS: i64 = 7 * 24 * 60 * 60;
const FAILED_RETRY_INTERVAL_SECONDS: i64 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChapterDetectionOptions {
    pub concurrency: i64,
    pub intro_window_seconds: i64,
    pub credits_window_seconds: i64,
    pub match_threshold: u32,
    pub force_refresh: bool,
}

impl Default for ChapterDetectionOptions {
    fn default() -> Self {
        Self {
            concurrency: 2,
            intro_window_seconds: 180,
            credits_window_seconds: 180,
            match_threshold: 80,
            force_refresh: false,
        }
    }
}

impl ChapterDetectionOptions {
    fn validate(self) -> Result<Self, ChapterDetectionError> {
        if !(1..=MAX_CONCURRENCY).contains(&self.concurrency)
            || !(MIN_WINDOW_SECONDS..=MAX_INTRO_WINDOW_SECONDS).contains(&self.intro_window_seconds)
            || !(MIN_WINDOW_SECONDS..=MAX_CREDITS_WINDOW_SECONDS)
                .contains(&self.credits_window_seconds)
            || !(1..=100).contains(&self.match_threshold)
        {
            return Err(ChapterDetectionError::InvalidOptions);
        }
        Ok(self)
    }

    fn threshold(self) -> f64 {
        f64::from(self.match_threshold) / 100.0
    }
}

#[derive(Clone)]
pub struct ChapterDetectionService {
    database: Database,
    plugins: PluginService,
    ffmpeg_binary: PathBuf,
    ffmpeg_timeout: Duration,
}

impl ChapterDetectionService {
    pub fn new(database: Database, plugins: PluginService) -> Self {
        Self {
            database,
            plugins,
            ffmpeg_binary: PathBuf::from(FFMPEG_BINARY),
            ffmpeg_timeout: FFMPEG_TIMEOUT,
        }
    }

    pub fn with_ffmpeg(mut self, binary: impl Into<PathBuf>, timeout: Duration) -> Self {
        self.ffmpeg_binary = binary.into();
        self.ffmpeg_timeout = timeout;
        self
    }

    pub async fn create_library_job(
        &self,
        library_id: LibraryId,
        plugin_id: &str,
        options: ChapterDetectionOptions,
    ) -> Result<ChapterDetectionJob, ChapterDetectionError> {
        let options = options.validate()?;
        let library_id_text = library_id.to_string();
        let library = self
            .database
            .find_library(&library_id_text)
            .await?
            .ok_or(ChapterDetectionError::LibraryNotFound)?;
        if !library.is_enabled {
            return Err(ChapterDetectionError::LibraryNotFound);
        }
        if library.kind == "MOVIE" {
            return Err(ChapterDetectionError::LibraryNotSupported);
        }
        if library.chapter_source_id.as_deref() != Some(plugin_id) {
            return Err(ChapterDetectionError::PluginUnavailable(
                plugin_id.to_owned(),
            ));
        }
        if !self
            .plugins
            .has_available_chapter_detector(plugin_id)
            .await?
        {
            return Err(ChapterDetectionError::PluginUnavailable(
                plugin_id.to_owned(),
            ));
        }
        if self
            .database
            .has_active_chapter_detection_job_for_library(&library_id_text)
            .await?
        {
            return Err(ChapterDetectionError::AlreadyActive);
        }
        let remote_lookup = self.plugins.is_chapter_lookup_plugin(plugin_id).await?;
        let candidates = self
            .eligible_sources(
                &library_id_text,
                plugin_id,
                remote_lookup,
                options,
                options.force_refresh,
            )
            .await?;
        let total_count = i64::try_from(
            candidates
                .iter()
                .filter(|source| !source.is_context)
                .count(),
        )
        .unwrap_or(i64::MAX);
        let id = Uuid::now_v7().to_string();
        let created = self
            .database
            .create_chapter_detection_job(NewChapterDetectionJob {
                id: &id,
                library_id: &library_id_text,
                plugin_id,
                concurrency: options.concurrency,
                intro_window_seconds: options.intro_window_seconds,
                credits_window_seconds: options.credits_window_seconds,
                match_threshold: options.threshold(),
                total_count,
            })
            .await?;
        if !created {
            return Err(ChapterDetectionError::AlreadyActive);
        }
        for page in candidates.chunks(SOURCE_PAGE_SIZE as usize) {
            let items = page
                .iter()
                .map(|source| NewChapterDetectionJobItem {
                    job_id: &id,
                    source_id: &source.source_id,
                    item_id: &source.item_id,
                    season_id: &source.season_id,
                    source_fingerprint: source.fingerprint.as_deref().unwrap_or(NO_FINGERPRINT),
                    input_fingerprint: &source.input_fingerprint,
                    is_context: source.is_context,
                })
                .collect::<Vec<_>>();
            if let Err(error) = self
                .database
                .insert_chapter_detection_job_items(&items)
                .await
            {
                let _ = self.database.delete_chapter_detection_job(&id).await;
                return Err(error.into());
            }
        }
        self.get(&id).await
    }

    async fn eligible_sources(
        &self,
        library_id: &str,
        plugin_id: &str,
        remote_lookup: bool,
        options: ChapterDetectionOptions,
        force_refresh: bool,
    ) -> Result<Vec<EligibleChapterDetectionSource>, ChapterDetectionError> {
        let mut sources = Vec::new();
        let mut after_source_id = None;
        loop {
            let page = self
                .database
                .list_chapter_detection_sources_page(
                    library_id,
                    plugin_id,
                    after_source_id.as_deref(),
                    SOURCE_PAGE_SIZE,
                    !remote_lookup,
                )
                .await?;
            let Some(last_source_id) = page.last().map(|source| source.source_id.clone()) else {
                break;
            };
            sources.extend(page);
            after_source_id = Some(last_source_id);
        }
        let minimum = minimum_episode_count(remote_lookup);
        let mut season_counts = HashMap::<String, usize>::new();
        for source in &sources {
            *season_counts.entry(source.season_id.clone()).or_default() += 1;
        }
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let mut candidates = sources
            .iter()
            .filter_map(|source| {
                if season_counts.get(&source.season_id).copied().unwrap_or(0) < minimum {
                    return None;
                }
                let input_fingerprint = chapter_input_fingerprint(source, remote_lookup, options)?;
                let state = source.state.as_ref();
                let input_changed =
                    state.is_none_or(|value| value.input_fingerprint != input_fingerprint);
                let retry_blocked = !force_refresh
                    && !input_changed
                    && state
                        .is_some_and(|value| value.next_retry_at.is_some_and(|retry| retry > now));
                if retry_blocked
                    || (!input_changed
                        && !should_refresh_source(
                            state.map(|value| value.status.as_str()),
                            state.map(|value| value.last_checked_at),
                            now,
                            force_refresh,
                        ))
                {
                    return None;
                }
                Some(EligibleChapterDetectionSource {
                    source_id: source.source_id.clone(),
                    item_id: source.item_id.clone(),
                    season_id: source.season_id.clone(),
                    fingerprint: source.fingerprint.clone(),
                    input_fingerprint,
                    is_context: false,
                })
            })
            .collect::<Vec<_>>();

        if !remote_lookup {
            let candidate_ids = candidates
                .iter()
                .map(|source| source.source_id.clone())
                .collect::<HashSet<_>>();
            let candidate_seasons = candidates.iter().filter(|source| !source.is_context).fold(
                HashMap::<String, usize>::new(),
                |mut counts, source| {
                    *counts.entry(source.season_id.clone()).or_default() += 1;
                    counts
                },
            );
            for (season_id, count) in candidate_seasons {
                if count >= 2 {
                    continue;
                }
                let Some(context) = sources.iter().find(|source| {
                    source.season_id == season_id
                        && !candidate_ids.contains(&source.source_id)
                        && source.state.as_ref().is_some_and(|state| {
                            state.intro_fingerprint.is_some()
                                && state.credits_fingerprint.is_some()
                                && chapter_input_fingerprint(source, false, options).is_some_and(
                                    |fingerprint| fingerprint == state.input_fingerprint,
                                )
                        })
                }) else {
                    continue;
                };
                let Some(input_fingerprint) = chapter_input_fingerprint(context, false, options)
                else {
                    continue;
                };
                candidates.push(EligibleChapterDetectionSource {
                    source_id: context.source_id.clone(),
                    item_id: context.item_id.clone(),
                    season_id: context.season_id.clone(),
                    fingerprint: context.fingerprint.clone(),
                    input_fingerprint,
                    is_context: true,
                });
            }
        }
        candidates.sort_by(|left, right| {
            left.season_id
                .cmp(&right.season_id)
                .then_with(|| left.source_id.cmp(&right.source_id))
        });
        Ok(candidates)
    }

    pub async fn get(&self, job_id: &str) -> Result<ChapterDetectionJob, ChapterDetectionError> {
        self.database
            .find_chapter_detection_job(job_id)
            .await?
            .map(chapter_detection_job)
            .ok_or(ChapterDetectionError::JobNotFound)
    }

    pub async fn list(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<ChapterDetectionJob>, ChapterDetectionError> {
        Ok(self
            .database
            .list_chapter_detection_jobs(status, offset, limit)
            .await?
            .into_iter()
            .map(chapter_detection_job)
            .collect())
    }

    pub async fn active_job_ids(&self) -> Result<Vec<String>, ChapterDetectionError> {
        Ok(self
            .database
            .list_active_chapter_detection_job_ids()
            .await?)
    }

    pub async fn cancel(&self, job_id: &str) -> Result<(), ChapterDetectionError> {
        let job = self
            .database
            .find_chapter_detection_job(job_id)
            .await?
            .ok_or(ChapterDetectionError::JobNotFound)?;
        if !matches!(job.status.as_str(), "PENDING" | "RUNNING") {
            return Err(ChapterDetectionError::NotCancellable);
        }
        self.database
            .request_chapter_detection_job_cancel(job_id)
            .await?;
        Ok(())
    }

    pub async fn retry(&self, job_id: &str) -> Result<ChapterDetectionJob, ChapterDetectionError> {
        let job = self
            .database
            .find_chapter_detection_job(job_id)
            .await?
            .ok_or(ChapterDetectionError::JobNotFound)?;
        if !matches!(job.status.as_str(), "FAILED" | "CANCELLED") {
            return Err(ChapterDetectionError::NotRetryable);
        }
        let options = ChapterDetectionOptions {
            concurrency: job.concurrency,
            intro_window_seconds: job.intro_window_seconds,
            credits_window_seconds: job.credits_window_seconds,
            match_threshold: (job.match_threshold * 100.0).round() as u32,
            force_refresh: true,
        };
        self.create_library_job(
            job.library_id
                .parse()
                .map_err(|_| ChapterDetectionError::LibraryNotFound)?,
            &job.plugin_id,
            options,
        )
        .await
    }

    pub async fn run(&self, job_id: &str) -> Result<(), ChapterDetectionError> {
        let job = self
            .database
            .find_chapter_detection_job(job_id)
            .await?
            .ok_or(ChapterDetectionError::JobNotFound)?;
        if job.status == "PENDING" && !self.database.claim_chapter_detection_job(job_id).await? {
            return Ok(());
        }
        if !matches!(job.status.as_str(), "PENDING" | "RUNNING") {
            return Ok(());
        }
        if job.status == "RUNNING" {
            self.database
                .requeue_running_chapter_detection_items(job_id)
                .await?;
        }
        self.run_claimed(job).await
    }

    async fn run_claimed(
        &self,
        job: StoredChapterDetectionJob,
    ) -> Result<(), ChapterDetectionError> {
        let concurrency = usize::try_from(job.concurrency)
            .unwrap_or(1)
            .clamp(1, MAX_CONCURRENCY as usize);
        let mut processed = job.processed_count.max(0).min(job.total_count.max(0));
        let mut failed = false;
        let mut cancelled = false;
        let remote_lookup = self
            .plugins
            .is_chapter_lookup_plugin(&job.plugin_id)
            .await?;
        let mut processed_source_ids = HashSet::new();
        let mut season_contexts = HashMap::<String, StoredChapterDetectionItem>::new();
        loop {
            if self
                .database
                .chapter_detection_job_cancel_requested(&job.id)
                .await?
            {
                cancelled = true;
                break;
            }
            let pending = self
                .database
                .list_pending_chapter_detection_items(&job.id, SOURCE_PAGE_SIZE)
                .await?;
            if pending.is_empty() {
                break;
            }
            let mut by_season = HashMap::<String, Vec<StoredChapterDetectionItem>>::new();
            for item in pending {
                by_season
                    .entry(item.season_id.clone())
                    .or_default()
                    .push(item);
            }
            for (season_id, mut season_items) in by_season {
                if let Some(context) = season_contexts.get(&season_id).cloned() {
                    season_items.insert(0, context);
                }
                let next_context = season_items.last().cloned();
                for (start, end) in chapter_batch_ranges_with_limit(
                    season_items.len(),
                    if remote_lookup {
                        MAX_REMOTE_EPISODES_PER_RPC
                    } else {
                        MAX_EPISODES_PER_RPC
                    },
                ) {
                    let items = &season_items[start..end];
                    if self
                        .database
                        .chapter_detection_job_cancel_requested(&job.id)
                        .await?
                    {
                        cancelled = true;
                        break;
                    }
                    let (outcomes, season_failed) = self
                        .process_season(&job, items.to_vec(), concurrency, &processed_source_ids)
                        .await?;
                    failed |= season_failed;
                    for outcome in outcomes {
                        if !processed_source_ids.insert(outcome.source_id.clone()) {
                            continue;
                        }
                        let status = if outcome.failed {
                            "FAILED"
                        } else if outcome.skipped {
                            "SKIPPED"
                        } else {
                            "COMPLETED"
                        };
                        self.database
                            .set_chapter_detection_item_status(
                                &job.id,
                                &outcome.source_id,
                                status,
                                outcome.error.as_deref(),
                            )
                            .await?;
                        self.record_source_outcome(&job.plugin_id, &outcome).await?;
                        if outcome.is_context {
                            continue;
                        }
                        processed = processed.saturating_add(1);
                        self.database
                            .update_chapter_detection_job_progress(
                                &job.id,
                                Some(&outcome.source_id),
                                processed,
                            )
                            .await?;
                    }
                }
                if cancelled {
                    break;
                }
                if let Some(context) = next_context {
                    season_contexts.insert(season_id, context);
                }
            }
            if cancelled {
                break;
            }
        }
        if self
            .database
            .chapter_detection_job_cancel_requested(&job.id)
            .await?
        {
            cancelled = true;
        }
        let (status, error) = if cancelled {
            ("CANCELLED", None)
        } else if failed {
            ("FAILED", Some(JOB_ERROR))
        } else {
            ("COMPLETED", None)
        };
        self.database
            .finish_chapter_detection_job(&job.id, status, error)
            .await?;
        Ok(())
    }

    async fn record_source_outcome(
        &self,
        plugin_id: &str,
        outcome: &SourceOutcome,
    ) -> Result<(), ChapterDetectionError> {
        if outcome.is_context || outcome.skipped || outcome.input_fingerprint.is_empty() {
            return Ok(());
        }
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let (status, last_success_at, next_retry_at, error) = if outcome.failed {
            (
                "FAILED",
                None,
                Some(now.saturating_add(FAILED_RETRY_INTERVAL_SECONDS)),
                outcome.error.as_deref(),
            )
        } else if outcome.markers_found {
            (
                "FOUND",
                Some(now),
                Some(now.saturating_add(FOUND_REFRESH_INTERVAL_SECONDS)),
                None,
            )
        } else {
            (
                "NOT_FOUND",
                None,
                Some(now.saturating_add(NOT_FOUND_RETRY_INTERVAL_SECONDS)),
                None,
            )
        };
        self.database
            .upsert_chapter_detection_source_state(
                &outcome.source_id,
                plugin_id,
                &outcome.input_fingerprint,
                status,
                now,
                last_success_at,
                next_retry_at,
                error,
                outcome.intro_fingerprint.as_deref(),
                outcome.credits_fingerprint.as_deref(),
            )
            .await?;
        Ok(())
    }

    async fn process_season(
        &self,
        job: &StoredChapterDetectionJob,
        items: Vec<StoredChapterDetectionItem>,
        concurrency: usize,
        protected_source_ids: &HashSet<String>,
    ) -> Result<(Vec<SourceOutcome>, bool), ChapterDetectionError> {
        if self
            .plugins
            .is_chapter_lookup_plugin(&job.plugin_id)
            .await?
        {
            return self
                .process_remote_season(job, items, protected_source_ids)
                .await;
        }
        if items.len() < 2 {
            return Ok((
                items.into_iter().map(SourceOutcome::skipped).collect(),
                false,
            ));
        }
        let semaphore = Arc::new(tokio::sync::Semaphore::new(concurrency));
        let mut pending = JoinSet::new();
        let job_template = job.clone();
        for item in items {
            let service = self.clone();
            let semaphore = semaphore.clone();
            let detector_job = job_template.clone();
            let source_id = item.source_id.clone();
            let input_fingerprint = item.input_fingerprint.clone();
            pending.spawn(async move {
                let permit = semaphore.acquire_owned().await;
                match permit {
                    Ok(_permit) => service.fingerprint_item(item, &detector_job).await,
                    Err(_) => SourceOutcome::failed_with_input(
                        &source_id,
                        &input_fingerprint,
                        "fingerprint worker stopped",
                    ),
                }
            });
        }
        let mut outcomes = Vec::new();
        while let Some(result) = pending.join_next().await {
            outcomes.push(result.map_err(|_| ChapterDetectionError::WorkerFailed)?);
        }
        let successful = outcomes
            .iter()
            .filter(|outcome| outcome.fingerprint_available)
            .collect::<Vec<_>>();
        let mut season_failed = outcomes
            .iter()
            .filter(|outcome| !protected_source_ids.contains(&outcome.source_id))
            .any(|outcome| outcome.failed);
        if successful.len() < 2 {
            for outcome in &mut outcomes {
                if outcome.fingerprint_available {
                    outcome.skipped = true;
                }
            }
        } else {
            let request = ChapterDetectRpcRequest {
                episodes: successful
                    .iter()
                    .map(|outcome| outcome.rpc_episode.clone())
                    .collect(),
                intro_window_ticks: job.intro_window_seconds.saturating_mul(10_000_000),
                credits_window_ticks: job.credits_window_seconds.saturating_mul(10_000_000),
                minimum_match_duration_ticks: MIN_MATCH_DURATION_TICKS,
                match_threshold: job.match_threshold,
            };
            match self.plugins.detect_chapters(&job.plugin_id, request).await {
                Ok(result) => {
                    let mut markers_by_key = HashMap::<String, Vec<ChapterDetectRpcMarker>>::new();
                    for marker in result.markers {
                        markers_by_key
                            .entry(marker.key.clone())
                            .or_default()
                            .push(marker);
                    }
                    for outcome in &mut outcomes {
                        if !outcome.fingerprint_available
                            || outcome.is_context
                            || protected_source_ids.contains(&outcome.source_id)
                        {
                            continue;
                        }
                        let markers = markers_by_key
                            .remove(&outcome.rpc_episode.key)
                            .unwrap_or_default();
                        match self
                            .persist_markers(outcome, markers, job.match_threshold, &job.plugin_id)
                            .await
                        {
                            Ok(found) => outcome.markers_found = found,
                            Err(error) => {
                                outcome.failed = true;
                                outcome.error = Some(error.to_string());
                                season_failed = true;
                            }
                        }
                    }
                }
                Err(error) => {
                    season_failed = true;
                    let message = sanitize_error(error.to_string());
                    for outcome in &mut outcomes {
                        if outcome.fingerprint_available {
                            outcome.failed = true;
                            outcome.error = Some(message.clone());
                        }
                    }
                }
            }
        }
        Ok((outcomes, season_failed))
    }

    async fn process_remote_season(
        &self,
        job: &StoredChapterDetectionJob,
        items: Vec<StoredChapterDetectionItem>,
        protected_source_ids: &HashSet<String>,
    ) -> Result<(Vec<SourceOutcome>, bool), ChapterDetectionError> {
        let mut outcomes = Vec::with_capacity(items.len());
        let mut episodes = Vec::new();
        for item in items {
            if protected_source_ids.contains(&item.source_id) {
                outcomes.push(SourceOutcome::skipped(item));
                continue;
            }
            let key = Uuid::now_v7().to_string();
            let Some(episode) = remote_lookup_episode(&item, key.clone()) else {
                outcomes.push(SourceOutcome::skipped(item));
                continue;
            };
            episodes.push(episode);
            outcomes.push(SourceOutcome::remote(item, key));
        }
        if episodes.is_empty() {
            return Ok((outcomes, false));
        }
        let request = ChapterLookupRpcRequest { episodes };
        let result = match self.plugins.lookup_chapters(&job.plugin_id, request).await {
            Ok(result) => result,
            Err(error) => {
                let message = sanitize_error(error.to_string());
                for outcome in &mut outcomes {
                    if outcome.fingerprint_available {
                        outcome.failed = true;
                        outcome.error = Some(message.clone());
                    }
                }
                return Ok((outcomes, true));
            }
        };
        let mut markers_by_key = HashMap::<String, Vec<ChapterDetectRpcMarker>>::new();
        for marker in result.markers {
            markers_by_key
                .entry(marker.key.clone())
                .or_default()
                .push(marker);
        }
        let mut season_failed = false;
        for outcome in &mut outcomes {
            if !outcome.fingerprint_available {
                continue;
            }
            let markers = markers_by_key
                .remove(&outcome.rpc_episode.key)
                .unwrap_or_default();
            match self
                .persist_markers(outcome, markers, job.match_threshold, &job.plugin_id)
                .await
            {
                Ok(found) => outcome.markers_found = found,
                Err(error) => {
                    outcome.failed = true;
                    outcome.error = Some(error.to_string());
                    season_failed = true;
                }
            }
        }
        Ok((outcomes, season_failed))
    }

    async fn fingerprint_item(
        &self,
        item: StoredChapterDetectionItem,
        job: &StoredChapterDetectionJob,
    ) -> SourceOutcome {
        if item.is_context {
            let Some(intro) = item.intro_fingerprint.clone() else {
                return SourceOutcome::failed_with_input(
                    &item.source_id,
                    &item.input_fingerprint,
                    "stored intro fingerprint is unavailable",
                );
            };
            let Some(credits) = item.credits_fingerprint.clone() else {
                return SourceOutcome::failed_with_input(
                    &item.source_id,
                    &item.input_fingerprint,
                    "stored credits fingerprint is unavailable",
                );
            };
            let Some(duration_ticks) = item.duration_ticks.filter(|duration| *duration > 0) else {
                return SourceOutcome::failed_with_input(
                    &item.source_id,
                    &item.input_fingerprint,
                    "media duration is unavailable",
                );
            };
            let intro_window_ticks = job
                .intro_window_seconds
                .saturating_mul(10_000_000)
                .min(duration_ticks);
            let credits_window_ticks = job
                .credits_window_seconds
                .saturating_mul(10_000_000)
                .min(duration_ticks);
            return SourceOutcome {
                source_id: item.source_id,
                source_fingerprint: item.source_fingerprint.unwrap_or_default(),
                input_fingerprint: item.input_fingerprint,
                rpc_episode: ChapterFingerprintRpcEpisode {
                    key: Uuid::now_v7().to_string(),
                    sample_rate: CHAPTER_FINGERPRINT_SAMPLE_RATE,
                    fingerprint_point_duration_ticks: CHAPTER_FINGERPRINT_POINT_DURATION_TICKS,
                    intro_fingerprint_base64: BASE64.encode(intro),
                    credits_fingerprint_base64: BASE64.encode(credits),
                    intro_window_start_ticks: 0,
                    credits_window_start_ticks: duration_ticks.saturating_sub(credits_window_ticks),
                    intro_window_duration_ticks: intro_window_ticks,
                    credits_window_duration_ticks: credits_window_ticks,
                },
                fingerprint_available: true,
                failed: false,
                skipped: false,
                is_context: true,
                markers_found: false,
                intro_fingerprint: None,
                credits_fingerprint: None,
                error: None,
            };
        }
        let key = Uuid::now_v7().to_string();
        let path = match safe_local_media_path(&item.root_path, &item.relative_path).await {
            Ok(path) => path,
            Err(error) => {
                return SourceOutcome::failed_for_item(item, &error.to_string());
            }
        };
        let Some(duration_ticks) = item.duration_ticks.filter(|duration| *duration > 0) else {
            return SourceOutcome::failed_for_item(item, "media duration is unavailable");
        };
        let intro_window_ticks = job
            .intro_window_seconds
            .saturating_mul(10_000_000)
            .min(duration_ticks);
        let credits_window_ticks = job
            .credits_window_seconds
            .saturating_mul(10_000_000)
            .min(duration_ticks);
        let credits_start_ticks = duration_ticks.saturating_sub(credits_window_ticks);
        let intro = match self
            .run_ffmpeg_fingerprint(&path, 0, intro_window_ticks)
            .await
        {
            Ok(bytes) => bytes,
            Err(error) => return SourceOutcome::failed_for_item(item, &error.to_string()),
        };
        let credits = match self
            .run_ffmpeg_fingerprint(&path, credits_start_ticks, credits_window_ticks)
            .await
        {
            Ok(bytes) => bytes,
            Err(error) => return SourceOutcome::failed_for_item(item, &error.to_string()),
        };
        SourceOutcome {
            source_id: item.source_id,
            source_fingerprint: item.source_fingerprint.unwrap_or_default(),
            input_fingerprint: item.input_fingerprint,
            rpc_episode: ChapterFingerprintRpcEpisode {
                key,
                sample_rate: CHAPTER_FINGERPRINT_SAMPLE_RATE,
                fingerprint_point_duration_ticks: CHAPTER_FINGERPRINT_POINT_DURATION_TICKS,
                intro_fingerprint_base64: BASE64.encode(&intro),
                credits_fingerprint_base64: BASE64.encode(&credits),
                intro_window_start_ticks: 0,
                credits_window_start_ticks: credits_start_ticks,
                intro_window_duration_ticks: intro_window_ticks,
                credits_window_duration_ticks: credits_window_ticks,
            },
            fingerprint_available: true,
            failed: false,
            skipped: false,
            is_context: false,
            markers_found: false,
            intro_fingerprint: Some(intro),
            credits_fingerprint: Some(credits),
            error: None,
        }
    }

    async fn run_ffmpeg_fingerprint(
        &self,
        path: &Path,
        start_ticks: i64,
        duration_ticks: i64,
    ) -> Result<Vec<u8>, FingerprintError> {
        let start = format_seconds(start_ticks);
        let duration = format_seconds(duration_ticks);
        let mut child = Command::new(&self.ffmpeg_binary)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-ss",
                &start,
                "-t",
                &duration,
                "-i",
            ])
            .arg(path)
            .args([
                "-map",
                "0:a:0?",
                "-vn",
                "-sn",
                "-dn",
                "-ar",
                "11025",
                "-ac",
                "1",
                "-f",
                "chromaprint",
                "-fp_format",
                "raw",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(FingerprintError::ProcessIo)?;
        let mut stdout = child.stdout.take().ok_or(FingerprintError::NoOutput)?;
        let read_result = timeout(
            self.ffmpeg_timeout,
            read_limited(&mut stdout, MAX_FINGERPRINT_BYTES),
        )
        .await
        .map_err(|_| FingerprintError::Timeout)??;
        let status = timeout(self.ffmpeg_timeout, child.wait())
            .await
            .map_err(|_| FingerprintError::Timeout)?
            .map_err(FingerprintError::ProcessIo)?;
        if !status.success() {
            return Err(FingerprintError::Exit(status.code()));
        }
        if read_result.is_empty() {
            return Err(FingerprintError::EmptyOutput);
        }
        canonicalize_raw_fingerprint(&read_result)
    }

    async fn persist_markers(
        &self,
        outcome: &SourceOutcome,
        markers: Vec<ChapterDetectRpcMarker>,
        threshold: f64,
        provider_id: &str,
    ) -> Result<bool, ChapterDetectionError> {
        if markers.is_empty() {
            return Ok(false);
        }
        let has_low_confidence = markers.iter().any(|marker| marker.confidence < threshold);
        if has_low_confidence {
            return Ok(false);
        }
        let mut seen = HashSet::new();
        let mut normalized = Vec::new();
        for marker in markers {
            let marker_name = marker.marker_type.name();
            if !seen.insert(marker_name) {
                return Err(ChapterDetectionError::InvalidPluginResult);
            }
            normalized.push(NewMediaChapterMarker {
                start_position_ticks: marker.start_position_ticks,
                name: marker
                    .name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned),
                marker_type: marker_name.to_owned(),
                chapter_index: marker.marker_type.index(),
                confidence: marker.confidence,
            });
        }
        normalized.sort_by_key(|marker| marker.chapter_index);
        let intro_start = normalized
            .iter()
            .find(|marker| marker.marker_type == "INTRO_START")
            .map(|marker| marker.start_position_ticks);
        let intro_end = normalized
            .iter()
            .find(|marker| marker.marker_type == "INTRO_END")
            .map(|marker| marker.start_position_ticks);
        let credits_start = normalized
            .iter()
            .find(|marker| marker.marker_type == "CREDITS_START")
            .map(|marker| marker.start_position_ticks);
        if intro_start
            .zip(intro_end)
            .is_some_and(|(start, end)| start >= end)
            || intro_end
                .zip(credits_start)
                .is_some_and(|(end, credits)| end > credits)
        {
            return Err(ChapterDetectionError::InvalidPluginResult);
        }
        let replaced = self
            .database
            .replace_detected_media_chapters(
                &outcome.source_id,
                provider_id,
                &outcome.source_fingerprint,
                &normalized,
            )
            .await
            .map_err(ChapterDetectionError::from)?;
        if !replaced {
            return Err(ChapterDetectionError::SourceChanged);
        }
        Ok(true)
    }
}

#[derive(Debug)]
struct SourceOutcome {
    source_id: String,
    source_fingerprint: Vec<u8>,
    input_fingerprint: Vec<u8>,
    rpc_episode: ChapterFingerprintRpcEpisode,
    fingerprint_available: bool,
    failed: bool,
    skipped: bool,
    is_context: bool,
    markers_found: bool,
    intro_fingerprint: Option<Vec<u8>>,
    credits_fingerprint: Option<Vec<u8>>,
    error: Option<String>,
}

struct EligibleChapterDetectionSource {
    source_id: String,
    item_id: String,
    season_id: String,
    fingerprint: Option<Vec<u8>>,
    input_fingerprint: Vec<u8>,
    is_context: bool,
}

impl SourceOutcome {
    fn remote(item: StoredChapterDetectionItem, key: String) -> Self {
        Self {
            source_id: item.source_id,
            source_fingerprint: item.source_fingerprint.unwrap_or_default(),
            input_fingerprint: item.input_fingerprint,
            rpc_episode: ChapterFingerprintRpcEpisode {
                key,
                sample_rate: CHAPTER_FINGERPRINT_SAMPLE_RATE,
                fingerprint_point_duration_ticks: CHAPTER_FINGERPRINT_POINT_DURATION_TICKS,
                intro_fingerprint_base64: String::new(),
                credits_fingerprint_base64: String::new(),
                intro_window_start_ticks: 0,
                credits_window_start_ticks: 0,
                intro_window_duration_ticks: 0,
                credits_window_duration_ticks: 0,
            },
            fingerprint_available: true,
            failed: false,
            skipped: false,
            is_context: item.is_context,
            markers_found: false,
            intro_fingerprint: None,
            credits_fingerprint: None,
            error: None,
        }
    }

    fn failed_with_input(source_id: &str, input_fingerprint: &[u8], error: &str) -> Self {
        Self {
            source_id: source_id.to_owned(),
            source_fingerprint: Vec::new(),
            input_fingerprint: input_fingerprint.to_vec(),
            rpc_episode: ChapterFingerprintRpcEpisode {
                key: String::new(),
                sample_rate: CHAPTER_FINGERPRINT_SAMPLE_RATE,
                fingerprint_point_duration_ticks: CHAPTER_FINGERPRINT_POINT_DURATION_TICKS,
                intro_fingerprint_base64: String::new(),
                credits_fingerprint_base64: String::new(),
                intro_window_start_ticks: 0,
                credits_window_start_ticks: 0,
                intro_window_duration_ticks: 0,
                credits_window_duration_ticks: 0,
            },
            fingerprint_available: false,
            failed: true,
            skipped: false,
            is_context: false,
            markers_found: false,
            intro_fingerprint: None,
            credits_fingerprint: None,
            error: Some(error.to_owned()),
        }
    }

    fn failed_for_item(item: StoredChapterDetectionItem, error: &str) -> Self {
        Self {
            source_id: item.source_id,
            source_fingerprint: item.source_fingerprint.unwrap_or_default(),
            input_fingerprint: item.input_fingerprint,
            rpc_episode: ChapterFingerprintRpcEpisode {
                key: String::new(),
                sample_rate: CHAPTER_FINGERPRINT_SAMPLE_RATE,
                fingerprint_point_duration_ticks: CHAPTER_FINGERPRINT_POINT_DURATION_TICKS,
                intro_fingerprint_base64: String::new(),
                credits_fingerprint_base64: String::new(),
                intro_window_start_ticks: 0,
                credits_window_start_ticks: 0,
                intro_window_duration_ticks: 0,
                credits_window_duration_ticks: 0,
            },
            fingerprint_available: false,
            failed: true,
            skipped: false,
            is_context: item.is_context,
            markers_found: false,
            intro_fingerprint: None,
            credits_fingerprint: None,
            error: Some(error.to_owned()),
        }
    }

    fn skipped(item: StoredChapterDetectionItem) -> Self {
        Self {
            source_id: item.source_id,
            source_fingerprint: item.source_fingerprint.unwrap_or_default(),
            input_fingerprint: item.input_fingerprint,
            rpc_episode: ChapterFingerprintRpcEpisode {
                key: String::new(),
                sample_rate: CHAPTER_FINGERPRINT_SAMPLE_RATE,
                fingerprint_point_duration_ticks: CHAPTER_FINGERPRINT_POINT_DURATION_TICKS,
                intro_fingerprint_base64: String::new(),
                credits_fingerprint_base64: String::new(),
                intro_window_start_ticks: 0,
                credits_window_start_ticks: 0,
                intro_window_duration_ticks: 0,
                credits_window_duration_ticks: 0,
            },
            fingerprint_available: false,
            failed: false,
            skipped: true,
            is_context: item.is_context,
            markers_found: false,
            intro_fingerprint: None,
            credits_fingerprint: None,
            error: None,
        }
    }
}

#[derive(Debug)]
enum FingerprintError {
    ProcessIo(std::io::Error),
    Timeout,
    Exit(Option<i32>),
    NoOutput,
    EmptyOutput,
    MalformedOutput,
    OutputTooLarge,
    InvalidPath(String),
}

impl fmt::Display for FingerprintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProcessIo(error) => {
                let _ = error;
                formatter.write_str("ffmpeg process failed")
            }
            Self::Timeout => formatter.write_str("ffmpeg fingerprint timed out"),
            Self::Exit(code) => {
                let _ = code;
                formatter.write_str("ffmpeg fingerprint failed")
            }
            Self::NoOutput | Self::EmptyOutput => {
                formatter.write_str("ffmpeg returned no fingerprint")
            }
            Self::MalformedOutput => formatter.write_str("ffmpeg returned malformed fingerprint"),
            Self::OutputTooLarge => formatter.write_str("ffmpeg fingerprint output is too large"),
            Self::InvalidPath(error) => {
                let _ = error;
                formatter.write_str("media path is invalid")
            }
        }
    }
}

impl std::error::Error for FingerprintError {}

fn canonicalize_raw_fingerprint(raw: &[u8]) -> Result<Vec<u8>, FingerprintError> {
    if raw.is_empty() {
        return Err(FingerprintError::EmptyOutput);
    }
    if raw.len() % std::mem::size_of::<u32>() != 0 {
        return Err(FingerprintError::MalformedOutput);
    }
    let mut canonical = Vec::with_capacity(raw.len());
    for point in raw.chunks_exact(std::mem::size_of::<u32>()) {
        let bytes: [u8; 4] = point
            .try_into()
            .map_err(|_| FingerprintError::MalformedOutput)?;
        canonical.extend_from_slice(&u32::from_ne_bytes(bytes).to_le_bytes());
    }
    Ok(canonical)
}

async fn read_limited<R: AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Vec<u8>, FingerprintError> {
    let mut result = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .await
            .map_err(FingerprintError::ProcessIo)?;
        if count == 0 {
            break;
        }
        if result.len().saturating_add(count) > max {
            return Err(FingerprintError::OutputTooLarge);
        }
        result.extend_from_slice(&buffer[..count]);
    }
    Ok(result)
}

async fn safe_local_media_path(
    root_path: &str,
    relative_path: &str,
) -> Result<PathBuf, FingerprintError> {
    let relative = Path::new(relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(FingerprintError::InvalidPath(relative_path.to_owned()));
    }
    let root = fs::canonicalize(root_path)
        .await
        .map_err(|error| FingerprintError::InvalidPath(error.to_string()))?;
    let path = fs::canonicalize(root.join(relative))
        .await
        .map_err(|error| FingerprintError::InvalidPath(error.to_string()))?;
    if !path.starts_with(&root) {
        return Err(FingerprintError::InvalidPath(relative_path.to_owned()));
    }
    let metadata = fs::metadata(&path)
        .await
        .map_err(|error| FingerprintError::InvalidPath(error.to_string()))?;
    if !metadata.is_file() {
        return Err(FingerprintError::InvalidPath(relative_path.to_owned()));
    }
    Ok(path)
}

fn format_seconds(ticks: i64) -> String {
    format!("{:.3}", ticks.max(0) as f64 / 10_000_000.0)
}

fn chapter_detection_job(job: StoredChapterDetectionJob) -> ChapterDetectionJob {
    ChapterDetectionJob {
        id: job.id,
        library_id: job.library_id,
        plugin_id: job.plugin_id,
        status: job.status,
        concurrency: job.concurrency,
        intro_window_seconds: job.intro_window_seconds,
        credits_window_seconds: job.credits_window_seconds,
        match_threshold: (job.match_threshold * 100.0).round() as u32,
        cursor: job.cursor,
        processed_count: job.processed_count,
        total_count: job.total_count,
        cancel_requested: job.cancel_requested,
        error: job.error,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterDetectionJob {
    pub id: String,
    pub library_id: String,
    pub plugin_id: String,
    pub status: String,
    pub concurrency: i64,
    pub intro_window_seconds: i64,
    pub credits_window_seconds: i64,
    pub match_threshold: u32,
    pub cursor: Option<String>,
    pub processed_count: i64,
    pub total_count: i64,
    pub cancel_requested: bool,
    pub error: Option<String>,
}

#[derive(Debug)]
pub enum ChapterDetectionError {
    InvalidOptions,
    AlreadyActive,
    LibraryNotFound,
    LibraryNotSupported,
    JobNotFound,
    NotCancellable,
    NotRetryable,
    WorkerFailed,
    InvalidPluginResult,
    SourceChanged,
    PluginUnavailable(String),
    Plugin(PluginServiceError),
    Storage(crate::storage::StorageError),
}

impl fmt::Display for ChapterDetectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOptions => formatter.write_str("invalid chapter detection options"),
            Self::AlreadyActive => formatter.write_str("a chapter detection job is already active"),
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::LibraryNotSupported => {
                formatter.write_str("chapter detection requires a series library")
            }
            Self::JobNotFound => formatter.write_str("chapter detection job not found"),
            Self::NotCancellable => formatter.write_str("chapter detection job is not cancellable"),
            Self::NotRetryable => formatter.write_str("chapter detection job is not retryable"),
            Self::WorkerFailed => formatter.write_str("chapter detection worker failed"),
            Self::InvalidPluginResult => {
                formatter.write_str("chapter detector returned an invalid result")
            }
            Self::SourceChanged => formatter.write_str("media source changed during detection"),
            Self::PluginUnavailable(plugin_id) => {
                write!(formatter, "chapter detector is unavailable: {plugin_id}")
            }
            Self::Plugin(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ChapterDetectionError {}

impl From<crate::storage::StorageError> for ChapterDetectionError {
    fn from(error: crate::storage::StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<PluginServiceError> for ChapterDetectionError {
    fn from(error: PluginServiceError) -> Self {
        Self::Plugin(error)
    }
}

impl ChapterDetectMarkerType {
    fn name(self) -> &'static str {
        match self {
            Self::IntroStart => "INTRO_START",
            Self::IntroEnd => "INTRO_END",
            Self::CreditsStart => "CREDITS_START",
        }
    }

    fn index(self) -> i64 {
        match self {
            Self::IntroStart => 0,
            Self::IntroEnd => 1,
            Self::CreditsStart => 2,
        }
    }
}

fn sanitize_error(value: String) -> String {
    let value = value.trim();
    if value.is_empty() {
        return JOB_ERROR.to_owned();
    }
    value.chars().take(512).collect()
}

fn remote_lookup_episode(
    item: &StoredChapterDetectionItem,
    key: String,
) -> Option<ChapterLookupRpcEpisode> {
    let item_ids = provider_ids(item.provider_ids_json.as_deref());
    let series_ids = provider_ids(item.series_provider_ids_json.as_deref());
    let tmdb_id = item_ids
        .tmdb
        .or(series_ids.tmdb)
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| (1..=2_000_000_000).contains(value));
    let tvdb_id = item_ids
        .tvdb
        .or(series_ids.tvdb)
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| (1..=2_000_000_000).contains(value));
    let imdb_id = item_ids.imdb.or(series_ids.imdb).filter(|value| {
        value.len() <= 32
            && value.starts_with("tt")
            && value[2..]
                .chars()
                .all(|character| character.is_ascii_digit())
    });
    let season_number = item
        .season_number
        .filter(|value| (0..=1000).contains(value))?;
    let episode_number = item
        .episode_number
        .filter(|value| (0..=10000).contains(value))?;
    (tmdb_id.is_some() || tvdb_id.is_some() || imdb_id.is_some()).then_some(
        ChapterLookupRpcEpisode {
            key,
            tmdb_id,
            tvdb_id,
            imdb_id,
            season_number,
            episode_number,
            duration_ticks: item
                .duration_ticks
                .filter(|value| (1..=3_600_000_000_000).contains(value)),
        },
    )
}

#[derive(Default)]
struct ProviderIds {
    tmdb: Option<String>,
    tvdb: Option<String>,
    imdb: Option<String>,
}

fn provider_ids(raw: Option<&str>) -> ProviderIds {
    let Some(raw) = raw else {
        return ProviderIds::default();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw) else {
        return ProviderIds::default();
    };
    let mut ids = ProviderIds::default();
    for (key, value) in value {
        let normalized = key.to_ascii_lowercase();
        let value = match value {
            serde_json::Value::String(value) => value,
            serde_json::Value::Number(value) => value.to_string(),
            _ => continue,
        };
        if value.trim().is_empty() {
            continue;
        }
        match normalized.as_str() {
            "tmdb" | "tmdbid" => ids.tmdb = Some(value),
            "tvdb" | "tvdbid" => ids.tvdb = Some(value),
            "imdb" | "imdbid" => ids.imdb = Some(value),
            _ => {}
        }
    }
    ids
}

fn chapter_input_fingerprint(
    source: &crate::storage::StoredChapterDetectionSource,
    remote_lookup: bool,
    options: ChapterDetectionOptions,
) -> Option<Vec<u8>> {
    let mut input = Vec::new();
    if remote_lookup {
        let ids = provider_ids(source.provider_ids_json.as_deref());
        let series_ids = provider_ids(source.series_provider_ids_json.as_deref());
        let has_provider = ids.tmdb.is_some()
            || ids.tvdb.is_some()
            || ids.imdb.is_some()
            || series_ids.tmdb.is_some()
            || series_ids.tvdb.is_some()
            || series_ids.imdb.is_some();
        if !has_provider || source.season_number.is_none() || source.episode_number.is_none() {
            return None;
        }
        input.extend_from_slice(b"remote\0");
        for value in [
            ids.tmdb,
            ids.tvdb,
            ids.imdb,
            series_ids.tmdb,
            series_ids.tvdb,
            series_ids.imdb,
        ] {
            input.extend_from_slice(value.as_deref().unwrap_or_default().as_bytes());
            input.push(0);
        }
        input.extend_from_slice(
            source
                .season_number
                .unwrap_or_default()
                .to_string()
                .as_bytes(),
        );
        input.push(0);
        input.extend_from_slice(
            source
                .episode_number
                .unwrap_or_default()
                .to_string()
                .as_bytes(),
        );
        input.push(0);
        input.extend_from_slice(
            source
                .duration_ticks
                .unwrap_or_default()
                .to_string()
                .as_bytes(),
        );
    } else {
        let fingerprint = source
            .fingerprint
            .as_deref()
            .filter(|value| !value.is_empty())?;
        input.extend_from_slice(b"local\0");
        input.extend_from_slice(fingerprint);
    }
    input.extend_from_slice(b"\0chapter-v1\0");
    input.extend_from_slice(options.intro_window_seconds.to_string().as_bytes());
    input.push(0);
    input.extend_from_slice(options.credits_window_seconds.to_string().as_bytes());
    Some(Sha256::digest(input).to_vec())
}

#[cfg(test)]
fn chapter_batch_ranges(item_count: usize) -> Vec<(usize, usize)> {
    chapter_batch_ranges_with_limit(item_count, MAX_EPISODES_PER_RPC)
}

fn chapter_batch_ranges_with_limit(item_count: usize, batch_size: usize) -> Vec<(usize, usize)> {
    if item_count == 0 {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < item_count {
        let end = start.saturating_add(batch_size.max(1)).min(item_count);
        ranges.push((start, end));
        if end == item_count {
            break;
        }
        start = end.saturating_sub(1);
    }
    ranges
}

fn minimum_episode_count(remote_lookup: bool) -> usize {
    if remote_lookup {
        REMOTE_MIN_EPISODES_PER_SEASON
    } else {
        LOCAL_MIN_EPISODES_PER_SEASON
    }
}

fn should_refresh_source(
    status: Option<&str>,
    last_checked_at: Option<i64>,
    now: i64,
    force_refresh: bool,
) -> bool {
    if force_refresh || status.is_none() || last_checked_at.is_none() {
        return true;
    }
    let age = now.saturating_sub(last_checked_at.unwrap_or(now));
    let interval = match status {
        Some("FOUND") => FOUND_REFRESH_INTERVAL_SECONDS,
        Some("NOT_FOUND") => NOT_FOUND_RETRY_INTERVAL_SECONDS,
        Some("FAILED") => FAILED_RETRY_INTERVAL_SECONDS,
        _ => 0,
    };
    age >= interval
}

#[cfg(test)]
mod tests {
    use super::{
        FingerprintError, StoredChapterDetectionItem, canonicalize_raw_fingerprint,
        chapter_batch_ranges, chapter_batch_ranges_with_limit, minimum_episode_count, provider_ids,
        remote_lookup_episode, should_refresh_source,
    };

    #[test]
    fn canonicalizes_native_raw_chromaprint_points_to_little_endian() {
        let mut raw = Vec::new();
        raw.extend_from_slice(&0x0102_0304_u32.to_ne_bytes());
        raw.extend_from_slice(&0xa0b0_c0d0_u32.to_ne_bytes());

        assert_eq!(
            canonicalize_raw_fingerprint(&raw).expect("valid raw points"),
            [0x0102_0304_u32.to_le_bytes(), 0xa0b0_c0d0_u32.to_le_bytes()].concat()
        );
    }

    #[test]
    fn rejects_incomplete_raw_chromaprint_point() {
        assert!(matches!(
            canonicalize_raw_fingerprint(&[1, 2, 3]),
            Err(FingerprintError::MalformedOutput)
        ));
    }

    #[test]
    fn chapter_batches_cover_every_episode_without_singletons() {
        for count in [2, 64, 65, 127, 128, 129, 257] {
            let ranges = chapter_batch_ranges(count);
            assert_eq!(ranges.first().map(|range| range.0), Some(0));
            assert_eq!(ranges.last().map(|range| range.1), Some(count));
            assert!(
                ranges
                    .iter()
                    .all(|(start, end)| { end > start && end - start <= 64 })
            );
            assert!(
                ranges
                    .windows(2)
                    .all(|window| window[0].1 - window[1].0 == 1)
            );
        }
        assert_eq!(chapter_batch_ranges(1), vec![(0, 1)]);
    }

    #[test]
    fn online_batches_are_bounded_for_rate_limited_requests() {
        let ranges = chapter_batch_ranges_with_limit(49, 24);
        assert_eq!(ranges, vec![(0, 24), (23, 47), (46, 49)]);
    }

    #[test]
    fn refresh_policy_uses_different_episode_gates_and_skips_fresh_results() {
        assert_eq!(minimum_episode_count(false), 3);
        assert_eq!(minimum_episode_count(true), 1);
        assert!(should_refresh_source(None, None, 100, false));
        assert!(!should_refresh_source(Some("FOUND"), Some(100), 200, false));
        assert!(should_refresh_source(
            Some("NOT_FOUND"),
            Some(100),
            7 * 24 * 60 * 60 + 100,
            false
        ));
        assert!(!should_refresh_source(
            Some("NOT_FOUND"),
            Some(200),
            100,
            false
        ));
        assert!(should_refresh_source(Some("FOUND"), Some(100), 200, true));
    }

    #[test]
    fn remote_lookup_uses_series_ids_when_episode_ids_are_missing() {
        let item = StoredChapterDetectionItem {
            source_id: "source".to_owned(),
            season_id: "season".to_owned(),
            source_fingerprint: Some(vec![1]),
            input_fingerprint: vec![1],
            is_context: false,
            intro_fingerprint: None,
            credits_fingerprint: None,
            duration_ticks: Some(1_800_000_000),
            root_path: "/media".to_owned(),
            relative_path: "episode.mkv".to_owned(),
            provider_ids_json: Some("{}".to_owned()),
            series_provider_ids_json: Some(
                r#"{"Tmdb":"123","Tvdb":"456","Imdb":"tt1234567"}"#.to_owned(),
            ),
            season_number: Some(1),
            episode_number: Some(2),
        };
        let request = remote_lookup_episode(&item, "key".to_owned()).expect("lookup episode");
        assert_eq!(request.tmdb_id, Some(123));
        assert_eq!(request.season_number, 1);
        assert_eq!(
            provider_ids(Some(r#"{"tmdb":123}"#)).tmdb.as_deref(),
            Some("123")
        );
    }
}
