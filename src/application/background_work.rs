use std::{
    collections::VecDeque,
    env,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::{OnceCell, OwnedSemaphorePermit, Semaphore, watch};

use crate::observability::resources::ResourceMetrics;

const BUDGET_ENV: &str = "LUX_BACKGROUND_CPU_BUDGET";
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
    semaphore: Arc<Semaphore>,
    metrics: Mutex<BudgetMetrics>,
}

impl BackgroundWorkBudget {
    fn with_capacity(capacity: usize, capacity_source: &'static str) -> Self {
        let capacity = capacity.clamp(1, MAX_BACKGROUND_BUDGET);
        Self {
            capacity,
            capacity_source,
            semaphore: Arc::new(Semaphore::new(capacity)),
            metrics: Mutex::new(BudgetMetrics::default()),
        }
    }

    pub(crate) async fn acquire(
        self: &Arc<Self>,
        stage: BackgroundWorkStage,
    ) -> Result<BackgroundWorkPermit, BackgroundWorkError> {
        let mut waiter = BackgroundWorkWaiter::new(Arc::clone(self), stage);
        let permit = Arc::clone(&self.semaphore)
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
}

impl Drop for BackgroundWorkPermit {
    fn drop(&mut self) {
        {
            let mut metrics = self.budget.lock_metrics();
            metrics.active = metrics.active.saturating_sub(1);
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
            Arc::new(BackgroundWorkBudget::with_capacity(capacity, source))
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
        BackgroundWorkBudget, BackgroundWorkStage, DurationSummary, parse_budget_override,
        recommended_budget_capacity, summarize_durations,
    };
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
}
