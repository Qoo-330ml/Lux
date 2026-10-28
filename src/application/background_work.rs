use std::{
    collections::VecDeque,
    env,
    future::Future,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use tokio::sync::{OnceCell, OwnedSemaphorePermit, Semaphore, watch};

use crate::observability::{request_metrics::RequestMetrics, resources::ResourceMetrics};

const BUDGET_ENV: &str = "LUX_BACKGROUND_CPU_BUDGET";
/// Opt-in switch for the foreground-latency feedback ("on", "1", "true", "yes").
/// Off by default: the quota change needs an interleaved A/B before it becomes the default.
const FOREGROUND_THROTTLE_ENV: &str = "LUX_BACKGROUND_FOREGROUND_THROTTLE";
/// Optional p95 threshold in milliseconds for that feedback.
const FOREGROUND_THRESHOLD_ENV: &str = "LUX_BACKGROUND_FOREGROUND_P95_MS";
const FOREGROUND_P95_ROUTE: &str = "/api/v1/home";
/// Read-model `off` keeps the legacy path, whose home p95 baseline is an order of magnitude higher.
const FOREGROUND_P95_OFF_MODE_MS: u64 = 300;
/// Read-model `on` answers the home page at 16–58 ms; the W3 scan-concurrency run measured 85.94 ms,
/// so this target has to sit well below the legacy default.
const FOREGROUND_P95_ON_MODE_MS: u64 = 80;
/// Only samples from this window count: the retained per-route buffer holds 512 samples, which on a
/// quiet server is minutes of traffic and would keep the downgrade engaged long after recovery.
const FOREGROUND_PRESSURE_WINDOW: Duration = Duration::from_secs(30);
const FOREGROUND_PRESSURE_RECHECK: Duration = Duration::from_secs(1);
const FOREGROUND_PRESSURE_COOLDOWN: Duration = Duration::from_secs(10);
const MAX_BACKGROUND_BUDGET: usize = 32;
const MAX_AUTOMATIC_BUDGET: usize = 16;
const DURATION_SAMPLE_CAPACITY: usize = 128;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum BackgroundWorkStage {
    Discovery,
    Fingerprint,
    ManifestPreparation,
    LocalNfo,
    LocalImage,
    Probe,
    Thumbnail,
}

impl BackgroundWorkStage {
    const ALL: [Self; 7] = [
        Self::Discovery,
        Self::Fingerprint,
        Self::ManifestPreparation,
        Self::LocalNfo,
        Self::LocalImage,
        Self::Probe,
        Self::Thumbnail,
    ];

    const fn index(self) -> usize {
        match self {
            Self::Discovery => 0,
            Self::Fingerprint => 1,
            Self::ManifestPreparation => 2,
            Self::LocalNfo => 3,
            Self::LocalImage => 4,
            Self::Probe => 5,
            Self::Thumbnail => 6,
        }
    }

    /// Stages that walk the filesystem share a lane with the other directory pipeline stages.
    const fn is_discovery_lane(self) -> bool {
        matches!(
            self,
            Self::Discovery | Self::Fingerprint | Self::ManifestPreparation
        )
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Fingerprint => "fingerprint",
            Self::ManifestPreparation => "manifest_preparation",
            Self::LocalNfo => "local_nfo",
            Self::LocalImage => "local_image",
            Self::Probe => "probe",
            Self::Thumbnail => "thumbnail",
        }
    }
}

/// Foreground-latency feedback for the background media lane.
///
/// The sample source is the read-only [`RequestMetrics::route_p95_recent_us`] window over the
/// requests per route template: when the foreground page p95 stays above the target, a share of the
/// media lane permits is held back so scans stop competing with the request path. Held permits are
/// released again only after a cooldown, and the lane keeps at least one permit, so the downgrade
/// bounds concurrency without changing any result or stopping background progress.
#[derive(Clone, Copy, Debug)]
struct ForegroundThrottleConfig {
    route_needle: &'static str,
    threshold_us: u64,
    window: Duration,
    recheck: Duration,
    cooldown: Duration,
}

/// Media-lane permits that stay available while foreground latency is above the target.
fn foreground_media_permits_kept(media_capacity: usize) -> usize {
    media_capacity.div_ceil(2).max(1)
}

fn default_foreground_threshold_ms(read_model_mode: Option<&str>) -> u64 {
    match read_model_mode
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("on") => FOREGROUND_P95_ON_MODE_MS,
        _ => FOREGROUND_P95_OFF_MODE_MS,
    }
}

fn foreground_throttle_config_from(
    switch: Option<&str>,
    threshold_ms: Option<&str>,
    read_model_mode: Option<&str>,
) -> Option<ForegroundThrottleConfig> {
    let switch = switch.unwrap_or_default().trim().to_ascii_lowercase();
    if !matches!(switch.as_str(), "on" | "1" | "true" | "yes") {
        return None;
    }
    let threshold_ms = threshold_ms
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(|| default_foreground_threshold_ms(read_model_mode));
    Some(ForegroundThrottleConfig {
        route_needle: FOREGROUND_P95_ROUTE,
        threshold_us: threshold_ms.saturating_mul(1000),
        window: FOREGROUND_PRESSURE_WINDOW,
        recheck: FOREGROUND_PRESSURE_RECHECK,
        cooldown: FOREGROUND_PRESSURE_COOLDOWN,
    })
}

fn foreground_throttle_config() -> Option<ForegroundThrottleConfig> {
    let switch = env::var(FOREGROUND_THROTTLE_ENV).ok();
    let threshold = env::var(FOREGROUND_THRESHOLD_ENV).ok();
    let read_model_mode = env::var("LUX_READ_MODEL_MODE").ok();
    let config = foreground_throttle_config_from(
        switch.as_deref(),
        threshold.as_deref(),
        read_model_mode.as_deref(),
    );
    if config.is_none()
        && switch
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        tracing::warn!(
            target: "lux::scan_performance",
            phase = "background_foreground_pressure",
            "unknown foreground throttle switch value; the feedback stays off"
        );
    }
    config
}

struct ForegroundThrottle {
    metrics: RequestMetrics,
    media_semaphore: Arc<Semaphore>,
    /// Permits held back once the foreground is slow: everything above the kept share.
    withheld_target: usize,
    config: ForegroundThrottleConfig,
    state: Mutex<ForegroundThrottleState>,
}

#[derive(Default)]
struct ForegroundThrottleState {
    withheld: Vec<OwnedSemaphorePermit>,
    over_since: Option<Instant>,
    last_evaluated: Option<Instant>,
}

impl ForegroundThrottle {
    fn new(
        media_semaphore: Arc<Semaphore>,
        media_capacity: usize,
        metrics: RequestMetrics,
        config: ForegroundThrottleConfig,
    ) -> Self {
        Self {
            metrics,
            media_semaphore,
            withheld_target: media_capacity
                .saturating_sub(foreground_media_permits_kept(media_capacity)),
            config,
            state: Mutex::new(ForegroundThrottleState::default()),
        }
    }

    /// Re-reads the foreground p95 and adjusts the held-back permits. Never awaits; the permit
    /// acquisition is best effort because every remaining permit may be busy with running work.
    fn observe(&self) {
        let now = Instant::now();
        let mut state = self.lock_state();
        if let Some(last) = state.last_evaluated
            && now.saturating_duration_since(last) < self.config.recheck
        {
            return;
        }
        state.last_evaluated = Some(now);
        let p95_us = self
            .metrics
            .route_p95_recent_us(self.config.route_needle, self.config.window);
        let slow = p95_us.is_some_and(|value| value >= self.config.threshold_us);
        if slow {
            let was_throttled = !state.withheld.is_empty();
            state.over_since = Some(now);
            while state.withheld.len() < self.withheld_target {
                let Ok(permit) = Arc::clone(&self.media_semaphore).try_acquire_owned() else {
                    break;
                };
                state.withheld.push(permit);
            }
            if !was_throttled && !state.withheld.is_empty() {
                tracing::info!(
                    target: "lux::scan_performance",
                    phase = "background_foreground_pressure",
                    route = self.config.route_needle,
                    p95_us = ?p95_us,
                    threshold_us = self.config.threshold_us,
                    withheld_permits = state.withheld.len(),
                    "withheld background media permits while the foreground page is slow"
                );
            }
            return;
        }
        let cooled_down = state
            .over_since
            .is_some_and(|since| now.saturating_duration_since(since) >= self.config.cooldown);
        if cooled_down && !state.withheld.is_empty() {
            state.over_since = None;
            let restored = state.withheld.len();
            state.withheld.clear();
            tracing::info!(
                target: "lux::scan_performance",
                phase = "background_foreground_pressure",
                route = self.config.route_needle,
                p95_us = ?p95_us,
                restored_permits = restored,
                "restored background media permits after the foreground cooldown"
            );
        }
    }

    fn snapshot(&self) -> (bool, usize) {
        let state = self.lock_state();
        (!state.withheld.is_empty(), state.withheld.len())
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, ForegroundThrottleState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BackgroundWorkSnapshot {
    pub(crate) capacity: usize,
    pub(crate) capacity_source: &'static str,
    pub(crate) active: usize,
    pub(crate) peak_active: usize,
    pub(crate) waiting: usize,
    pub(crate) peak_waiting: usize,
    pub(crate) cancelled_waits: u64,
    pub(crate) cancelled_running: usize,
    pub(crate) child_cleanup_pending: usize,
    pub(crate) cancelled_waits_by_stage: Vec<(&'static str, u64)>,
    pub(crate) cancelled_running_by_stage: Vec<(&'static str, usize)>,
    pub(crate) child_cleanup_pending_by_stage: Vec<(&'static str, usize)>,
    pub(crate) active_by_stage: Vec<(&'static str, usize, usize)>,
    pub(crate) waiting_by_stage: Vec<(&'static str, usize, usize)>,
    pub(crate) duration_by_stage: Vec<(&'static str, DurationSummary, DurationSummary)>,
    pub(crate) wait_p50_ms: Option<u64>,
    pub(crate) wait_p95_ms: Option<u64>,
    pub(crate) wait_max_ms: Option<u64>,
    pub(crate) work_p50_ms: Option<u64>,
    pub(crate) work_p95_ms: Option<u64>,
    pub(crate) work_max_ms: Option<u64>,
    /// Foreground-latency feedback: whether media permits are currently held back, and how many.
    pub(crate) foreground_throttled: bool,
    pub(crate) foreground_withheld_permits: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct DurationSummary {
    pub(crate) p50_ms: Option<u64>,
    pub(crate) p95_ms: Option<u64>,
    pub(crate) max_ms: Option<u64>,
}

#[derive(Default)]
struct StageMetrics {
    active: usize,
    peak_active: usize,
    waiting: usize,
    peak_waiting: usize,
    cancelled_waits: u64,
    cancelled_running: usize,
    child_cleanup_pending: usize,
    wait_ms: VecDeque<u64>,
    work_ms: VecDeque<u64>,
}

struct BudgetMetrics {
    active: usize,
    peak_active: usize,
    waiting: usize,
    peak_waiting: usize,
    cancelled_waits: u64,
    cancelled_running: usize,
    child_cleanup_pending: usize,
    stages: [StageMetrics; 7],
}

impl Default for BudgetMetrics {
    fn default() -> Self {
        Self {
            active: 0,
            peak_active: 0,
            waiting: 0,
            peak_waiting: 0,
            cancelled_waits: 0,
            cancelled_running: 0,
            child_cleanup_pending: 0,
            stages: std::array::from_fn(|_| StageMetrics::default()),
        }
    }
}

pub(crate) struct BackgroundWorkBudget {
    capacity: usize,
    capacity_source: &'static str,
    /// Directory walking, fingerprinting, and manifest preparation can stall on a slow mount;
    /// keeping them in their own lane stops them from starving probe/NFO/thumbnail work.
    discovery_semaphore: Arc<Semaphore>,
    media_semaphore: Arc<Semaphore>,
    media_capacity: usize,
    /// Installed once, either when the budget is created or when the web app registers its
    /// request metrics, whichever happens last.
    foreground: OnceLock<ForegroundThrottle>,
    metrics: Mutex<BudgetMetrics>,
}

impl BackgroundWorkBudget {
    fn with_capacity(capacity: usize, capacity_source: &'static str) -> Self {
        let capacity = capacity.clamp(1, MAX_BACKGROUND_BUDGET);
        // Three fifths for the directory pipeline, the rest for per-item media work. A single
        // slot cannot be split, so both lanes share it.
        let (discovery_capacity, media_capacity) = if capacity < 2 {
            (capacity, capacity)
        } else {
            let discovery = (capacity * 3 / 5).max(1);
            (discovery, (capacity - discovery).max(1))
        };
        let discovery_semaphore = Arc::new(Semaphore::new(discovery_capacity));
        let media_semaphore = if capacity < 2 {
            Arc::clone(&discovery_semaphore)
        } else {
            Arc::new(Semaphore::new(media_capacity))
        };
        Self {
            capacity,
            capacity_source,
            discovery_semaphore,
            media_semaphore,
            media_capacity,
            foreground: OnceLock::new(),
            metrics: Mutex::new(BudgetMetrics::default()),
        }
    }

    /// Installs the foreground-latency feedback once per budget.
    ///
    /// Idempotent: the first installation wins, so a budget created before the web app registers
    /// its request metrics and one created after both end up with the same feedback.
    fn attach_foreground_pressure(
        &self,
        metrics: RequestMetrics,
        config: ForegroundThrottleConfig,
    ) {
        // A single-permit budget shares one semaphore between both lanes; it cannot be split.
        let retained = foreground_media_permits_kept(self.media_capacity);
        if self.media_capacity < 2 || retained >= self.media_capacity {
            return;
        }
        let throttle = ForegroundThrottle::new(
            Arc::clone(&self.media_semaphore),
            self.media_capacity,
            metrics,
            config,
        );
        if self.foreground.set(throttle).is_err() {
            return;
        }
        tracing::info!(
            target: "lux::scan_performance",
            phase = "background_foreground_pressure",
            route = config.route_needle,
            threshold_us = config.threshold_us,
            cooldown_ms = u64::try_from(config.cooldown.as_millis()).unwrap_or(u64::MAX),
            media_capacity = self.media_capacity,
            kept_permits = retained,
            withheld_permits = self.media_capacity - retained,
            "enabled foreground latency feedback for the background media lane"
        );
    }

    pub(crate) async fn acquire(
        self: &Arc<Self>,
        stage: BackgroundWorkStage,
    ) -> Result<BackgroundWorkPermit, BackgroundWorkError> {
        let mut waiter = BackgroundWorkWaiter::new(Arc::clone(self), stage);
        let semaphore = if stage.is_discovery_lane() {
            Arc::clone(&self.discovery_semaphore)
        } else {
            if let Some(throttle) = self.foreground.get() {
                throttle.observe();
            }
            Arc::clone(&self.media_semaphore)
        };
        let permit = semaphore
            .acquire_owned()
            .await
            .map_err(|_| BackgroundWorkError::Closed)?;
        Ok(waiter.acquired(permit))
    }

    async fn run_blocking<T, F>(
        self: &Arc<Self>,
        stage: BackgroundWorkStage,
        work: F,
    ) -> Result<T, BackgroundWorkTaskError>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let permit = self
            .acquire(stage)
            .await
            .map_err(BackgroundWorkTaskError::Budget)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(BackgroundWorkTaskError::Join)
    }

    async fn run_supervised<T, F, Fut>(
        self: &Arc<Self>,
        stage: BackgroundWorkStage,
        work: F,
    ) -> Result<T, BackgroundWorkTaskError>
    where
        T: Send + 'static,
        F: FnOnce(BackgroundWorkCancellation) -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let permit = self
            .acquire(stage)
            .await
            .map_err(BackgroundWorkTaskError::Budget)?;
        let state = Arc::new(SupervisedTaskState::new(Arc::clone(self), stage));
        let (cancel_sender, cancel_receiver) = watch::channel(false);
        let caller_guard = SupervisedCallerGuard {
            state: Arc::clone(&state),
            cancel_sender,
        };
        let task_state = Arc::clone(&state);
        let result = tokio::spawn(async move {
            let _permit = permit;
            let _completion = SupervisedCompletionGuard(task_state);
            work(BackgroundWorkCancellation {
                receiver: cancel_receiver,
            })
            .await
        })
        .await
        .map_err(BackgroundWorkTaskError::Join);
        drop(caller_guard);
        result
    }

    fn begin_child_cleanup(
        self: &Arc<Self>,
        stage: BackgroundWorkStage,
    ) -> BackgroundChildCleanupPending {
        {
            let mut metrics = self.lock_metrics();
            metrics.child_cleanup_pending = metrics.child_cleanup_pending.saturating_add(1);
            let stage_metrics = &mut metrics.stages[stage.index()];
            stage_metrics.child_cleanup_pending =
                stage_metrics.child_cleanup_pending.saturating_add(1);
        }
        BackgroundChildCleanupPending {
            budget: Arc::clone(self),
            stage,
        }
    }

    pub(crate) fn snapshot(&self) -> BackgroundWorkSnapshot {
        let (foreground_throttled, foreground_withheld_permits) = self
            .foreground
            .get()
            .map_or((false, 0), ForegroundThrottle::snapshot);
        let metrics = self.lock_metrics();
        let wait_samples = BackgroundWorkStage::ALL
            .iter()
            .flat_map(|stage| metrics.stages[stage.index()].wait_ms.iter().copied())
            .collect::<VecDeque<_>>();
        let work_samples = BackgroundWorkStage::ALL
            .iter()
            .flat_map(|stage| metrics.stages[stage.index()].work_ms.iter().copied())
            .collect::<VecDeque<_>>();
        let wait_summary = summarize_durations(&wait_samples);
        let work_summary = summarize_durations(&work_samples);
        BackgroundWorkSnapshot {
            capacity: self.capacity,
            capacity_source: self.capacity_source,
            active: metrics.active,
            peak_active: metrics.peak_active,
            waiting: metrics.waiting,
            peak_waiting: metrics.peak_waiting,
            cancelled_waits: metrics.cancelled_waits,
            cancelled_running: metrics.cancelled_running,
            child_cleanup_pending: metrics.child_cleanup_pending,
            cancelled_waits_by_stage: BackgroundWorkStage::ALL
                .into_iter()
                .map(|stage| {
                    (
                        stage.as_str(),
                        metrics.stages[stage.index()].cancelled_waits,
                    )
                })
                .collect(),
            cancelled_running_by_stage: BackgroundWorkStage::ALL
                .into_iter()
                .map(|stage| {
                    (
                        stage.as_str(),
                        metrics.stages[stage.index()].cancelled_running,
                    )
                })
                .collect(),
            child_cleanup_pending_by_stage: BackgroundWorkStage::ALL
                .into_iter()
                .map(|stage| {
                    (
                        stage.as_str(),
                        metrics.stages[stage.index()].child_cleanup_pending,
                    )
                })
                .collect(),
            active_by_stage: BackgroundWorkStage::ALL
                .into_iter()
                .map(|stage| {
                    let metrics = &metrics.stages[stage.index()];
                    (stage.as_str(), metrics.active, metrics.peak_active)
                })
                .collect(),
            waiting_by_stage: BackgroundWorkStage::ALL
                .into_iter()
                .map(|stage| {
                    let metrics = &metrics.stages[stage.index()];
                    (stage.as_str(), metrics.waiting, metrics.peak_waiting)
                })
                .collect(),
            duration_by_stage: BackgroundWorkStage::ALL
                .into_iter()
                .map(|stage| {
                    let metrics = &metrics.stages[stage.index()];
                    (
                        stage.as_str(),
                        summarize_durations(&metrics.wait_ms),
                        summarize_durations(&metrics.work_ms),
                    )
                })
                .collect(),
            wait_p50_ms: wait_summary.p50_ms,
            wait_p95_ms: wait_summary.p95_ms,
            wait_max_ms: wait_summary.max_ms,
            work_p50_ms: work_summary.p50_ms,
            work_p95_ms: work_summary.p95_ms,
            work_max_ms: work_summary.max_ms,
            foreground_throttled,
            foreground_withheld_permits,
        }
    }

    fn lock_metrics(&self) -> std::sync::MutexGuard<'_, BudgetMetrics> {
        self.metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

struct BackgroundWorkWaiter {
    budget: Arc<BackgroundWorkBudget>,
    stage: BackgroundWorkStage,
    started_at: Instant,
    finished: bool,
}

impl BackgroundWorkWaiter {
    fn new(budget: Arc<BackgroundWorkBudget>, stage: BackgroundWorkStage) -> Self {
        {
            let mut metrics = budget.lock_metrics();
            metrics.waiting = metrics.waiting.saturating_add(1);
            metrics.peak_waiting = metrics.peak_waiting.max(metrics.waiting);
            let stage_metrics = &mut metrics.stages[stage.index()];
            stage_metrics.waiting = stage_metrics.waiting.saturating_add(1);
            stage_metrics.peak_waiting = stage_metrics.peak_waiting.max(stage_metrics.waiting);
        }
        Self {
            budget,
            stage,
            started_at: Instant::now(),
            finished: false,
        }
    }

    fn acquired(&mut self, permit: OwnedSemaphorePermit) -> BackgroundWorkPermit {
        let wait_duration = self.started_at.elapsed();
        {
            let mut metrics = self.budget.lock_metrics();
            metrics.waiting = metrics.waiting.saturating_sub(1);
            metrics.active = metrics.active.saturating_add(1);
            metrics.peak_active = metrics.peak_active.max(metrics.active);
            let stage_metrics = &mut metrics.stages[self.stage.index()];
            stage_metrics.waiting = stage_metrics.waiting.saturating_sub(1);
            stage_metrics.active = stage_metrics.active.saturating_add(1);
            stage_metrics.peak_active = stage_metrics.peak_active.max(stage_metrics.active);
            push_duration(&mut stage_metrics.wait_ms, wait_duration);
        }
        self.finished = true;
        BackgroundWorkPermit {
            permit: Some(permit),
            budget: Arc::clone(&self.budget),
            stage: self.stage,
            started_at: Instant::now(),
            wait_duration,
            cancelled_running: false,
        }
    }
}

impl Drop for BackgroundWorkWaiter {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut metrics = self.budget.lock_metrics();
        metrics.waiting = metrics.waiting.saturating_sub(1);
        metrics.cancelled_waits = metrics.cancelled_waits.saturating_add(1);
        let stage_metrics = &mut metrics.stages[self.stage.index()];
        stage_metrics.waiting = stage_metrics.waiting.saturating_sub(1);
        stage_metrics.cancelled_waits = stage_metrics.cancelled_waits.saturating_add(1);
    }
}

pub(crate) struct BackgroundWorkPermit {
    permit: Option<OwnedSemaphorePermit>,
    budget: Arc<BackgroundWorkBudget>,
    stage: BackgroundWorkStage,
    started_at: Instant,
    wait_duration: Duration,
    cancelled_running: bool,
}

impl BackgroundWorkPermit {
    pub(crate) fn mark_cancelled_running(&mut self) {
        if self.cancelled_running {
            return;
        }
        let mut metrics = self.budget.lock_metrics();
        metrics.cancelled_running = metrics.cancelled_running.saturating_add(1);
        let stage_metrics = &mut metrics.stages[self.stage.index()];
        stage_metrics.cancelled_running = stage_metrics.cancelled_running.saturating_add(1);
        self.cancelled_running = true;
    }
}

impl Drop for BackgroundWorkPermit {
    fn drop(&mut self) {
        {
            let mut metrics = self.budget.lock_metrics();
            metrics.active = metrics.active.saturating_sub(1);
            if self.cancelled_running {
                metrics.cancelled_running = metrics.cancelled_running.saturating_sub(1);
                let stage_metrics = &mut metrics.stages[self.stage.index()];
                stage_metrics.cancelled_running = stage_metrics.cancelled_running.saturating_sub(1);
            }
            let stage_metrics = &mut metrics.stages[self.stage.index()];
            stage_metrics.active = stage_metrics.active.saturating_sub(1);
            push_duration(&mut stage_metrics.work_ms, self.started_at.elapsed());
        }
        drop(self.permit.take());
        let work_duration = self.started_at.elapsed();
        if self.wait_duration >= Duration::from_millis(5)
            || work_duration >= Duration::from_millis(5)
        {
            let snapshot = self.budget.snapshot();
            let stage_snapshot = snapshot
                .active_by_stage
                .iter()
                .find(|(stage, _, _)| *stage == self.stage.as_str());
            tracing::debug!(
                target: "lux::scan_performance",
                phase = "background_work_budget_wait",
                stage = self.stage.as_str(),
                wait_ms = u64::try_from(self.wait_duration.as_millis()).unwrap_or(u64::MAX),
                capacity = snapshot.capacity,
                capacity_source = snapshot.capacity_source,
                active = snapshot.active,
                active_peak = snapshot.peak_active,
                stage_active = stage_snapshot.map(|(_, active, _)| *active).unwrap_or(0),
                stage_active_peak = stage_snapshot.map(|(_, _, peak)| *peak).unwrap_or(0),
                waiting = snapshot.waiting,
                waiting_peak = snapshot.peak_waiting,
                cancelled_running = snapshot.cancelled_running,
                cancelled_waits = snapshot.cancelled_waits,
                child_cleanup_pending = snapshot.child_cleanup_pending,
                stage_waiting = ?snapshot.waiting_by_stage,
                stage_cancelled_waits = ?snapshot.cancelled_waits_by_stage,
                stage_cancelled_running = ?snapshot.cancelled_running_by_stage,
                stage_child_cleanup_pending = ?snapshot.child_cleanup_pending_by_stage,
                stage_durations = ?snapshot.duration_by_stage,
                wait_p50_ms = ?snapshot.wait_p50_ms,
                wait_p95_ms = ?snapshot.wait_p95_ms,
                wait_max_ms = ?snapshot.wait_max_ms,
                work_p50_ms = ?snapshot.work_p50_ms,
                work_p95_ms = ?snapshot.work_p95_ms,
                work_max_ms = ?snapshot.work_max_ms,
                "background work stage completed"
            );
        }
    }
}

struct SupervisedTaskState {
    budget: Arc<BackgroundWorkBudget>,
    stage: BackgroundWorkStage,
    state: Mutex<SupervisedTaskStatus>,
}

#[derive(Default)]
struct SupervisedTaskStatus {
    finished: bool,
    caller_cancelled: bool,
}

impl SupervisedTaskState {
    fn new(budget: Arc<BackgroundWorkBudget>, stage: BackgroundWorkStage) -> Self {
        Self {
            budget,
            stage,
            state: Mutex::new(SupervisedTaskStatus::default()),
        }
    }

    fn caller_cancelled(&self) -> bool {
        let mut state = self.lock_state();
        if state.finished || state.caller_cancelled {
            return false;
        }
        state.caller_cancelled = true;
        let mut metrics = self.budget.lock_metrics();
        metrics.cancelled_running = metrics.cancelled_running.saturating_add(1);
        metrics.stages[self.stage.index()].cancelled_running = metrics.stages[self.stage.index()]
            .cancelled_running
            .saturating_add(1);
        tracing::debug!(
            target: "lux::scan_performance",
            phase = "background_work_cancelled_while_running",
            stage = self.stage.as_str(),
            capacity = self.budget.capacity,
            cancelled_running = metrics.cancelled_running,
            "caller cancelled while supervised background work continues"
        );
        true
    }

    fn finished(&self) {
        let mut state = self.lock_state();
        if state.finished {
            return;
        }
        state.finished = true;
        if state.caller_cancelled {
            let mut metrics = self.budget.lock_metrics();
            metrics.cancelled_running = metrics.cancelled_running.saturating_sub(1);
            metrics.stages[self.stage.index()].cancelled_running = metrics.stages
                [self.stage.index()]
            .cancelled_running
            .saturating_sub(1);
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SupervisedTaskStatus> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

struct SupervisedCallerGuard {
    state: Arc<SupervisedTaskState>,
    cancel_sender: watch::Sender<bool>,
}

impl Drop for SupervisedCallerGuard {
    fn drop(&mut self) {
        if self.state.caller_cancelled() {
            let _ = self.cancel_sender.send(true);
        }
    }
}

pub(crate) struct BackgroundWorkCancellation {
    receiver: watch::Receiver<bool>,
}

impl BackgroundWorkCancellation {
    pub(crate) async fn cancelled(&mut self) {
        loop {
            if *self.receiver.borrow_and_update() {
                return;
            }
            if self.receiver.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }
}

struct SupervisedCompletionGuard(Arc<SupervisedTaskState>);

impl Drop for SupervisedCompletionGuard {
    fn drop(&mut self) {
        self.0.finished();
    }
}

pub(crate) struct BackgroundChildCleanupPending {
    budget: Arc<BackgroundWorkBudget>,
    stage: BackgroundWorkStage,
}

impl BackgroundChildCleanupPending {
    pub(crate) fn count(&self) -> usize {
        self.budget.lock_metrics().stages[self.stage.index()].child_cleanup_pending
    }
}

impl Drop for BackgroundChildCleanupPending {
    fn drop(&mut self) {
        let mut metrics = self.budget.lock_metrics();
        metrics.child_cleanup_pending = metrics.child_cleanup_pending.saturating_sub(1);
        metrics.stages[self.stage.index()].child_cleanup_pending = metrics.stages
            [self.stage.index()]
        .child_cleanup_pending
        .saturating_sub(1);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackgroundWorkError {
    Closed,
}

impl std::fmt::Display for BackgroundWorkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => formatter.write_str("background work budget is closed"),
        }
    }
}

impl std::error::Error for BackgroundWorkError {}

#[derive(Debug)]
pub(crate) enum BackgroundWorkTaskError {
    Budget(BackgroundWorkError),
    Join(tokio::task::JoinError),
}

impl std::fmt::Display for BackgroundWorkTaskError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Budget(error) => write!(formatter, "{error}"),
            Self::Join(error) => write!(formatter, "background work task failed: {error}"),
        }
    }
}

impl std::error::Error for BackgroundWorkTaskError {}

static PROCESS_BACKGROUND_WORK_BUDGET: OnceCell<Arc<BackgroundWorkBudget>> = OnceCell::const_new();
/// Bounded route samples for the foreground-latency feedback; registered once by the web app.
static FOREGROUND_REQUEST_METRICS: OnceLock<RequestMetrics> = OnceLock::new();

/// Registers the process request metrics so background work can read the foreground p95.
///
/// The budget is a process singleton and the first registration wins, so a second app state in the
/// same process keeps reporting into the first one's samples. The feedback itself stays off unless
/// it was opted in, in which case an already-created budget is upgraded here.
pub(crate) fn install_foreground_request_metrics(metrics: RequestMetrics) {
    if FOREGROUND_REQUEST_METRICS.set(metrics.clone()).is_err() {
        return;
    }
    if let Some(budget) = PROCESS_BACKGROUND_WORK_BUDGET.get()
        && let Some(config) = foreground_throttle_config()
    {
        budget.attach_foreground_pressure(metrics, config);
    }
}

pub(crate) async fn acquire_process_background_work(
    stage: BackgroundWorkStage,
) -> Result<BackgroundWorkPermit, BackgroundWorkError> {
    process_background_work_budget().await.acquire(stage).await
}

async fn process_background_work_budget() -> Arc<BackgroundWorkBudget> {
    Arc::clone(
        PROCESS_BACKGROUND_WORK_BUDGET
        .get_or_init(|| async {
            let visible_parallelism = std::thread::available_parallelism()
                .map(|value| value.get())
                .unwrap_or(1);
            let cgroup_limit = ResourceMetrics::new().cpu_limit_cores().await;
            let automatic_capacity = recommended_budget_capacity(visible_parallelism, cgroup_limit);
            let (capacity, source) = match env::var(BUDGET_ENV) {
                Ok(value) => match parse_budget_override(&value) {
                    Ok(Some(capacity)) => (capacity, "environment"),
                    Ok(None) => (automatic_capacity, "automatic"),
                    Err(error_code) => {
                        tracing::warn!(
                            error_code,
                            "invalid background CPU budget; using the automatic capacity"
                        );
                        (automatic_capacity, "automatic")
                    }
                },
                Err(env::VarError::NotPresent) => (automatic_capacity, "automatic"),
                Err(env::VarError::NotUnicode(_)) => {
                    tracing::warn!(
                        "background CPU budget is not valid Unicode; using the automatic capacity"
                    );
                    (automatic_capacity, "automatic")
                }
            };
            tracing::info!(
                target: "lux::scan_performance",
                phase = "background_work_budget_configuration",
                capacity,
                visible_parallelism,
                cgroup_limit_cores = ?cgroup_limit,
                source,
                "initialized process-wide background work budget"
            );
            let budget = Arc::new(BackgroundWorkBudget::with_capacity(capacity, source));
            if let Some(metrics) = FOREGROUND_REQUEST_METRICS.get()
                && let Some(config) = foreground_throttle_config()
            {
                budget.attach_foreground_pressure(metrics.clone(), config);
            }
            budget
        })
            .await,
    )
}

pub(crate) async fn process_background_child_cleanup_pending(
    stage: BackgroundWorkStage,
) -> BackgroundChildCleanupPending {
    process_background_work_budget()
        .await
        .begin_child_cleanup(stage)
}

pub(crate) async fn run_process_background_blocking<T, F>(
    stage: BackgroundWorkStage,
    work: F,
) -> Result<T, BackgroundWorkTaskError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let budget = process_background_work_budget().await;
    budget.run_blocking(stage, work).await
}

pub(crate) async fn run_process_background_supervised<T, F, Fut>(
    stage: BackgroundWorkStage,
    work: F,
) -> Result<T, BackgroundWorkTaskError>
where
    T: Send + 'static,
    F: FnOnce(BackgroundWorkCancellation) -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
{
    process_background_work_budget()
        .await
        .run_supervised(stage, work)
        .await
}

fn recommended_budget_capacity(visible_parallelism: usize, cgroup_limit: Option<f64>) -> usize {
    let visible_parallelism = visible_parallelism.max(1);
    let capacity = cgroup_limit
        .filter(|value| value.is_finite() && *value > 0.0)
        .map_or(visible_parallelism, |limit| {
            visible_parallelism.min(limit.floor().max(1.0) as usize)
        });
    capacity.saturating_sub(1).clamp(1, MAX_AUTOMATIC_BUDGET)
}

fn parse_budget_override(value: &str) -> Result<Option<usize>, &'static str> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    let parsed = value
        .trim()
        .parse::<usize>()
        .map_err(|_| "not_an_unsigned_integer")?;
    Ok(Some(parsed.clamp(1, MAX_BACKGROUND_BUDGET)))
}

fn push_duration(samples: &mut VecDeque<u64>, duration: Duration) {
    if samples.len() == DURATION_SAMPLE_CAPACITY {
        samples.pop_front();
    }
    samples.push_back(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
}

fn summarize_durations(samples: &VecDeque<u64>) -> DurationSummary {
    if samples.is_empty() {
        return DurationSummary::default();
    }
    let mut sorted = samples.iter().copied().collect::<Vec<_>>();
    sorted.sort_unstable();
    let nearest_rank = |percentile: usize| {
        sorted
            .get((sorted.len() * percentile).div_ceil(100).saturating_sub(1))
            .copied()
    };
    DurationSummary {
        p50_ms: nearest_rank(50),
        p95_ms: nearest_rank(95),
        max_ms: sorted.last().copied(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BackgroundWorkBudget, BackgroundWorkStage, DurationSummary, ForegroundThrottleConfig,
        foreground_media_permits_kept, foreground_throttle_config_from, parse_budget_override,
        recommended_budget_capacity, summarize_durations,
    };
    use crate::observability::request_metrics::RequestMetrics;
    use std::{collections::VecDeque, sync::Arc, time::Duration};
    use tokio::{sync::Semaphore, time::timeout};

    #[test]
    fn default_budget_reserves_one_cpu_and_has_a_safe_cap() {
        assert_eq!(recommended_budget_capacity(4, None), 3);
        assert_eq!(recommended_budget_capacity(8, None), 7);
        assert_eq!(recommended_budget_capacity(32, None), 16);
    }

    #[test]
    fn default_budget_honors_fractional_container_limits_and_minimum() {
        assert_eq!(recommended_budget_capacity(8, Some(1.5)), 1);
        assert_eq!(recommended_budget_capacity(8, Some(4.0)), 3);
        assert_eq!(recommended_budget_capacity(1, None), 1);
        assert_eq!(recommended_budget_capacity(8, Some(f64::NAN)), 7);
    }

    #[test]
    fn explicit_budget_is_clamped_and_invalid_values_are_rejected() {
        assert_eq!(
            parse_budget_override(" ").expect("empty uses automatic"),
            None
        );
        assert_eq!(parse_budget_override("0").expect("zero clamps"), Some(1));
        assert_eq!(
            parse_budget_override("12").expect("valid override"),
            Some(12)
        );
        assert_eq!(
            parse_budget_override("99").expect("large values clamp"),
            Some(32)
        );
        assert_eq!(parse_budget_override("bad"), Err("not_an_unsigned_integer"));
        assert_eq!(parse_budget_override("-1"), Err("not_an_unsigned_integer"));
    }

    #[test]
    fn duration_summary_uses_nearest_rank_and_preserves_empty_samples() {
        let samples = VecDeque::from([1, 2, 5, 9]);
        assert_eq!(
            summarize_durations(&samples),
            DurationSummary {
                p50_ms: Some(2),
                p95_ms: Some(9),
                max_ms: Some(9),
            }
        );
        assert_eq!(
            summarize_durations(&VecDeque::new()),
            DurationSummary::default()
        );
    }

    #[test]
    fn child_cleanup_pending_is_counted_until_reaping_finishes() {
        let budget = Arc::new(BackgroundWorkBudget::with_capacity(1, "test"));
        let cleanup = budget.begin_child_cleanup(BackgroundWorkStage::Thumbnail);
        assert_eq!(cleanup.count(), 1);
        assert_eq!(budget.snapshot().child_cleanup_pending, 1);
        assert_eq!(budget.snapshot().child_cleanup_pending_by_stage[6].1, 1);
        drop(cleanup);
        assert_eq!(budget.snapshot().child_cleanup_pending, 0);
        assert_eq!(budget.snapshot().child_cleanup_pending_by_stage[6].1, 0);
    }

    #[tokio::test]
    async fn cancelled_process_retains_its_stage_count_until_its_permit_drops() {
        let budget = Arc::new(BackgroundWorkBudget::with_capacity(1, "test"));
        let mut permit = budget
            .acquire(BackgroundWorkStage::Probe)
            .await
            .expect("probe permit should be granted");

        permit.mark_cancelled_running();
        assert_eq!(budget.snapshot().active, 1);
        assert_eq!(budget.snapshot().cancelled_running, 1);
        assert_eq!(budget.snapshot().cancelled_running_by_stage[5].1, 1);

        drop(permit);
        assert_eq!(budget.snapshot().active, 0);
        assert_eq!(budget.snapshot().cancelled_running, 0);
        assert_eq!(budget.snapshot().cancelled_running_by_stage[5].1, 0);
    }

    #[tokio::test]
    async fn cloned_budget_limits_active_work_across_pipeline_stages() {
        let budget = Arc::new(BackgroundWorkBudget::with_capacity(2, "test"));
        let started = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let mut workers = Vec::new();
        for index in 0..BackgroundWorkStage::ALL.len() {
            let budget = Arc::clone(&budget);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            workers.push(tokio::spawn(async move {
                let stage = BackgroundWorkStage::ALL[index];
                let permit = budget.acquire(stage).await.expect("budget open");
                started.add_permits(1);
                let released = release.acquire().await.expect("release signal");
                released.forget();
                drop(permit);
            }));
        }

        timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = budget.snapshot();
                if snapshot.active == 2 && snapshot.waiting == 5 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("workers reach the shared limit");
        assert_eq!(budget.snapshot().peak_active, 2);

        for _ in 0..BackgroundWorkStage::ALL.len() {
            let entered = timeout(Duration::from_secs(2), started.acquire())
                .await
                .expect("worker starts")
                .expect("start signal");
            entered.forget();
            release.add_permits(1);
        }
        for worker in workers {
            worker.await.expect("worker task");
        }
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.active, 0);
        assert_eq!(snapshot.peak_active, 2);
        assert_eq!(snapshot.capacity_source, "test");
        assert!(snapshot.wait_p50_ms.is_some());
        assert!(snapshot.work_p50_ms.is_some());
        assert_eq!(snapshot.active_by_stage[0].0, "discovery");
        assert_eq!(snapshot.active_by_stage[5].0, "probe");
    }

    #[tokio::test]
    async fn a_saturated_discovery_lane_does_not_starve_media_work() {
        let budget = Arc::new(BackgroundWorkBudget::with_capacity(4, "test"));
        let mut discovery_permits = Vec::new();
        for _ in 0..2 {
            discovery_permits.push(
                budget
                    .acquire(BackgroundWorkStage::Discovery)
                    .await
                    .expect("discovery permit"),
            );
        }
        let waiting_budget = Arc::clone(&budget);
        let blocked = tokio::spawn(async move {
            waiting_budget
                .acquire(BackgroundWorkStage::Discovery)
                .await
                .map(|_permit| ())
        });

        // The media lane keeps its own share even while the directory lane is full.
        let media_permit = budget
            .acquire(BackgroundWorkStage::Probe)
            .await
            .expect("probe permit");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), blocked)
                .await
                .is_err(),
            "a third discovery permit must wait for the lane"
        );
        drop(media_permit);
        drop(discovery_permits);
    }

    #[tokio::test]
    async fn cancelled_waiter_leaves_capacity_and_wait_metrics_consistent() {
        let budget = Arc::new(BackgroundWorkBudget::with_capacity(1, "test"));
        let held = budget
            .acquire(BackgroundWorkStage::LocalNfo)
            .await
            .expect("budget open");
        let waiting_budget = Arc::clone(&budget);
        let waiting =
            tokio::spawn(
                async move { waiting_budget.acquire(BackgroundWorkStage::Thumbnail).await },
            );
        timeout(Duration::from_secs(2), async {
            while budget.snapshot().waiting == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("second task waits for capacity");

        waiting.abort();
        let _ = waiting.await;
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.waiting, 0);
        assert_eq!(snapshot.cancelled_waits, 1);
        assert_eq!(snapshot.cancelled_waits_by_stage[6].1, 1);
        assert_eq!(snapshot.active, 1);
        drop(held);

        let recovered = timeout(
            Duration::from_secs(2),
            budget.acquire(BackgroundWorkStage::Thumbnail),
        )
        .await
        .expect("capacity is released")
        .expect("budget open");
        assert_eq!(budget.snapshot().active, 1);
        drop(recovered);
    }

    #[tokio::test]
    async fn cancelled_caller_keeps_budget_until_blocking_work_finishes() {
        let budget = Arc::new(BackgroundWorkBudget::with_capacity(1, "test"));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocking_budget = Arc::clone(&budget);
        let caller = tokio::spawn(async move {
            blocking_budget
                .run_blocking(BackgroundWorkStage::Discovery, move || {
                    let _ = entered_tx.send(());
                    release_rx.recv().expect("test releases blocking work");
                })
                .await
        });
        entered_rx.await.expect("blocking work started");
        caller.abort();
        let _ = caller.await;

        let waiting_budget = Arc::clone(&budget);
        let waiting = tokio::spawn(async move {
            waiting_budget
                .acquire(BackgroundWorkStage::Fingerprint)
                .await
        });
        timeout(Duration::from_secs(2), async {
            while budget.snapshot().waiting == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shared capacity remains occupied");
        assert_eq!(budget.snapshot().active, 1);

        release_tx.send(()).expect("blocking worker is waiting");
        let next_permit = timeout(Duration::from_secs(2), waiting)
            .await
            .expect("waiting task acquires after blocking work exits")
            .expect("waiting task");
        assert_eq!(budget.snapshot().active, 1);
        drop(next_permit.expect("budget open"));
        assert_eq!(budget.snapshot().active, 0);
    }

    #[tokio::test]
    async fn cancelled_caller_keeps_budget_until_supervised_task_finishes() {
        let budget = Arc::new(BackgroundWorkBudget::with_capacity(1, "test"));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (cancelled_tx, cancelled_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let supervised_budget = Arc::clone(&budget);
        let caller = tokio::spawn(async move {
            supervised_budget
                .run_supervised(
                    BackgroundWorkStage::Probe,
                    move |mut cancellation| async move {
                        let _ = entered_tx.send(());
                        cancellation.cancelled().await;
                        let _ = cancelled_tx.send(());
                        let _ = release_rx.await;
                    },
                )
                .await
        });
        entered_rx.await.expect("supervised task started");
        caller.abort();
        let _ = caller.await;
        cancelled_rx
            .await
            .expect("supervised task observed caller cancellation");

        let waiting_budget = Arc::clone(&budget);
        let waiting =
            tokio::spawn(
                async move { waiting_budget.acquire(BackgroundWorkStage::Thumbnail).await },
            );
        timeout(Duration::from_secs(2), async {
            while budget.snapshot().waiting == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("shared capacity remains occupied");
        assert_eq!(budget.snapshot().active, 1);
        assert_eq!(budget.snapshot().cancelled_running, 1);
        assert_eq!(budget.snapshot().cancelled_running_by_stage[5].1, 1);

        release_tx.send(()).expect("supervised task is waiting");
        let next_permit = timeout(Duration::from_secs(2), waiting)
            .await
            .expect("waiting task acquires after supervised work exits")
            .expect("waiting task");
        assert_eq!(budget.snapshot().active, 1);
        drop(next_permit.expect("budget open"));
        assert_eq!(budget.snapshot().active, 0);
        assert_eq!(budget.snapshot().cancelled_running, 0);
        assert_eq!(budget.snapshot().cancelled_running_by_stage[5].1, 0);
    }

    fn record_home_samples(metrics: &RequestMetrics, count: usize, ms: u64) {
        for _ in 0..count {
            metrics.record("GET", "/api/v1/home", 200, Duration::from_millis(ms));
        }
    }

    fn test_foreground_throttle_config(cooldown: Duration) -> ForegroundThrottleConfig {
        ForegroundThrottleConfig {
            route_needle: "/api/v1/home",
            threshold_us: 300_000,
            window: Duration::from_secs(30),
            recheck: Duration::ZERO,
            cooldown,
        }
    }

    #[test]
    fn foreground_throttle_is_off_unless_explicitly_enabled() {
        assert!(foreground_throttle_config_from(None, None, None).is_none());
        assert!(foreground_throttle_config_from(Some(""), None, None).is_none());
        assert!(foreground_throttle_config_from(Some(" off "), None, None).is_none());
        assert!(foreground_throttle_config_from(Some("0"), None, None).is_none());

        let enabled =
            foreground_throttle_config_from(Some("on"), None, Some("off")).expect("on enables");
        assert_eq!(enabled.route_needle, "/api/v1/home");
        assert_eq!(enabled.threshold_us, 300_000);
        assert_eq!(enabled.window, Duration::from_secs(30));

        let tuned =
            foreground_throttle_config_from(Some("ON"), Some("75"), None).expect("on enables");
        assert_eq!(tuned.threshold_us, 75_000);
        for invalid in ["0", "abc", ""] {
            let fallback = foreground_throttle_config_from(Some("on"), Some(invalid), None)
                .expect("on enables");
            assert_eq!(
                fallback.threshold_us, 300_000,
                "{invalid} keeps the default"
            );
        }
    }

    #[test]
    fn foreground_threshold_follows_the_read_model_mode() {
        // The legacy path needs a much looser target than the read-model path.
        for (mode, expected_us) in [
            (Some("on"), 80_000),
            (Some("ON"), 80_000),
            (Some("off"), 300_000),
            (Some("shadow"), 300_000),
            (None, 300_000),
        ] {
            let config = foreground_throttle_config_from(Some("on"), None, mode).expect("enabled");
            assert_eq!(config.threshold_us, expected_us, "mode {mode:?}");
        }
        // An explicit override wins in both modes.
        for mode in [Some("on"), Some("off")] {
            let config =
                foreground_throttle_config_from(Some("on"), Some("120"), mode).expect("enabled");
            assert_eq!(config.threshold_us, 120_000, "mode {mode:?}");
        }
    }

    #[test]
    fn foreground_pressure_keeps_at_least_one_media_permit() {
        for capacity in 1..=32 {
            let kept = foreground_media_permits_kept(capacity);
            assert!(kept >= 1, "capacity {capacity} keeps one permit");
            assert!(
                kept <= capacity,
                "capacity {capacity} never invents permits"
            );
            assert!(
                kept <= capacity.div_ceil(2),
                "capacity {capacity} at most halves"
            );
        }
    }

    #[tokio::test]
    async fn foreground_pressure_halves_the_media_lane_and_restores_after_the_cooldown() {
        let metrics = RequestMetrics::new();
        record_home_samples(&metrics, 32, 400);
        // Capacity 4 splits into 2 discovery + 2 media; throttling keeps 1 media permit.
        let budget = Arc::new(BackgroundWorkBudget::with_capacity(4, "test"));
        budget.attach_foreground_pressure(
            metrics.clone(),
            test_foreground_throttle_config(Duration::from_millis(40)),
        );

        let first = budget
            .acquire(BackgroundWorkStage::Probe)
            .await
            .expect("probe permit");
        let throttled = budget.snapshot();
        assert!(throttled.foreground_throttled);
        assert_eq!(throttled.foreground_withheld_permits, 1);

        let blocked_budget = Arc::clone(&budget);
        let mut blocked =
            tokio::spawn(async move { blocked_budget.acquire(BackgroundWorkStage::Probe).await });
        assert!(
            timeout(Duration::from_millis(100), &mut blocked)
                .await
                .is_err(),
            "a throttled media lane must not grant the second probe"
        );
        blocked.abort();
        let _ = blocked.await;

        // Foreground recovers: only fast samples remain and the cooldown elapses.
        record_home_samples(&metrics, 600, 5);
        tokio::time::sleep(Duration::from_millis(60)).await;
        drop(first);

        let second = timeout(
            Duration::from_secs(2),
            budget.acquire(BackgroundWorkStage::Probe),
        )
        .await
        .expect("acquire after the cooldown")
        .expect("budget open");
        let restored = budget.snapshot();
        assert!(!restored.foreground_throttled);
        assert_eq!(restored.foreground_withheld_permits, 0);
        let third = timeout(
            Duration::from_secs(2),
            budget.acquire(BackgroundWorkStage::Probe),
        )
        .await
        .expect("both media permits are available again")
        .expect("budget open");
        drop(third);
        drop(second);
    }

    #[tokio::test]
    async fn budgets_without_foreground_pressure_keep_the_media_lane() {
        let budget = Arc::new(BackgroundWorkBudget::with_capacity(4, "test"));
        let first = budget
            .acquire(BackgroundWorkStage::Probe)
            .await
            .expect("probe permit");
        let second = timeout(
            Duration::from_secs(1),
            budget.acquire(BackgroundWorkStage::Probe),
        )
        .await
        .expect("both media permits stay available")
        .expect("budget open");
        let snapshot = budget.snapshot();
        assert!(!snapshot.foreground_throttled);
        assert_eq!(snapshot.foreground_withheld_permits, 0);
        drop(second);
        drop(first);
    }
}
