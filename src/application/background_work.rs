use std::{
    collections::VecDeque,
    env,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::sync::{OnceCell, OwnedSemaphorePermit, Semaphore};

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
    pub(crate) active: usize,
    pub(crate) peak_active: usize,
    pub(crate) waiting: usize,
    pub(crate) peak_waiting: usize,
    pub(crate) cancelled_waits: u64,
    pub(crate) active_by_stage: Vec<(&'static str, usize, usize)>,
    pub(crate) wait_p95_ms: Option<u64>,
    pub(crate) work_p95_ms: Option<u64>,
}

#[derive(Default)]
struct StageMetrics {
    active: usize,
    peak_active: usize,
    waiting: usize,
    peak_waiting: usize,
    wait_ms: VecDeque<u64>,
    work_ms: VecDeque<u64>,
}

struct BudgetMetrics {
    active: usize,
    peak_active: usize,
    waiting: usize,
    peak_waiting: usize,
    cancelled_waits: u64,
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
            stages: std::array::from_fn(|_| StageMetrics::default()),
        }
    }
}

pub(crate) struct BackgroundWorkBudget {
    capacity: usize,
    semaphore: Arc<Semaphore>,
    metrics: Mutex<BudgetMetrics>,
}

impl BackgroundWorkBudget {
    fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.clamp(1, MAX_BACKGROUND_BUDGET);
        Self {
            capacity,
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

    pub(crate) fn snapshot(&self) -> BackgroundWorkSnapshot {
        let metrics = self.lock_metrics();
        BackgroundWorkSnapshot {
            capacity: self.capacity,
            active: metrics.active,
            peak_active: metrics.peak_active,
            waiting: metrics.waiting,
            peak_waiting: metrics.peak_waiting,
            cancelled_waits: metrics.cancelled_waits,
            active_by_stage: BackgroundWorkStage::ALL
                .into_iter()
                .map(|stage| {
                    let metrics = &metrics.stages[stage.index()];
                    (stage.as_str(), metrics.active, metrics.peak_active)
                })
                .collect(),
            wait_p95_ms: percentile95(
                &BackgroundWorkStage::ALL
                    .iter()
                    .flat_map(|stage| metrics.stages[stage.index()].wait_ms.iter().copied())
                    .collect(),
            ),
            work_p95_ms: percentile95(
                &BackgroundWorkStage::ALL
                    .iter()
                    .flat_map(|stage| metrics.stages[stage.index()].work_ms.iter().copied())
                    .collect(),
            ),
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
        metrics.stages[self.stage.index()].waiting =
            metrics.stages[self.stage.index()].waiting.saturating_sub(1);
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
        if self.wait_duration >= Duration::from_millis(5) {
            tracing::debug!(
                target: "lux::scan_performance",
                phase = "background_work_budget_wait",
                stage = self.stage.as_str(),
                wait_ms = u64::try_from(self.wait_duration.as_millis()).unwrap_or(u64::MAX),
                capacity = self.budget.capacity,
                "background work waited for a shared CPU permit"
            );
        }
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

static PROCESS_BACKGROUND_WORK_BUDGET: OnceCell<Arc<BackgroundWorkBudget>> = OnceCell::const_new();

pub(crate) async fn acquire_process_background_work(
    stage: BackgroundWorkStage,
) -> Result<BackgroundWorkPermit, BackgroundWorkError> {
    let budget = PROCESS_BACKGROUND_WORK_BUDGET
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
                cgroup_limit_cores = cgroup_limit.unwrap_or(0.0),
                source,
                "initialized process-wide background work budget"
            );
            Arc::new(BackgroundWorkBudget::with_capacity(capacity))
        })
        .await;
    budget.acquire(stage).await
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

fn percentile95(samples: &VecDeque<u64>) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.iter().copied().collect::<Vec<_>>();
    sorted.sort_unstable();
    sorted
        .get((sorted.len() * 95).div_ceil(100).saturating_sub(1))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::{
        BackgroundWorkBudget, BackgroundWorkStage, parse_budget_override,
        recommended_budget_capacity,
    };
    use std::{sync::Arc, time::Duration};
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

    #[tokio::test]
    async fn cloned_budget_limits_active_work_across_pipeline_stages() {
        let budget = Arc::new(BackgroundWorkBudget::with_capacity(2));
        let started = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let mut workers = Vec::new();
        for index in 0..6 {
            let budget = Arc::clone(&budget);
            let started = Arc::clone(&started);
            let release = Arc::clone(&release);
            workers.push(tokio::spawn(async move {
                let stage = if index % 2 == 0 {
                    BackgroundWorkStage::Discovery
                } else {
                    BackgroundWorkStage::Probe
                };
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
                if snapshot.active == 2 && snapshot.waiting == 4 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("workers reach the shared limit");
        assert_eq!(budget.snapshot().peak_active, 2);

        for _ in 0..6 {
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
        assert_eq!(snapshot.active_by_stage[0].0, "discovery");
        assert_eq!(snapshot.active_by_stage[5].0, "probe");
    }

    #[tokio::test]
    async fn cancelled_waiter_leaves_capacity_and_wait_metrics_consistent() {
        let budget = Arc::new(BackgroundWorkBudget::with_capacity(1));
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
}
