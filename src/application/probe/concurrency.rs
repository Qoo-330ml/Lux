use std::{
    collections::{HashMap, VecDeque},
    fmt,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::Notify;

use crate::{domain::ids::LibraryId, observability::resources::ProbePressure};

use super::{MAX_EFFECTIVE_PROBE_CONCURRENCY, adaptive};
use adaptive::PressureLevel;

const WINDOW_MIN: Duration = Duration::from_secs(10);
const WINDOW_MAX: Duration = Duration::from_secs(30);
const TICK_INTERVAL: Duration = Duration::from_secs(1);
const LATENCY_SAMPLE_CAPACITY: usize = 256;
const IDLE_EPOCH_RESET: Duration = Duration::from_secs(60);

static GLOBAL_PROBE_RUNTIME: ProbeRuntimeRegistry = ProbeRuntimeRegistry::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AttemptOutcome {
    Success,
    Failure,
    Timeout,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdmissionError {
    Closed,
    LockPoisoned,
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => formatter.write_str("probe admission is closed"),
            Self::LockPoisoned => formatter.write_str("probe admission state is unavailable"),
        }
    }
}

#[derive(Clone)]
pub(super) struct ProbeRuntime {
    inner: Arc<ProbeRuntimeInner>,
}

struct ProbeRuntimeInner {
    state: Mutex<AdmissionState>,
    changed: Notify,
    next_session_id: AtomicU64,
    next_attempt_id: AtomicU64,
}

struct ProbeRuntimeRegistry {
    lifecycle: Mutex<()>,
    closing: AtomicBool,
    runtime: OnceLock<ProbeRuntime>,
}

impl ProbeRuntimeRegistry {
    const fn new() -> Self {
        Self {
            lifecycle: Mutex::new(()),
            closing: AtomicBool::new(false),
            runtime: OnceLock::new(),
        }
    }

    fn get_or_init(
        &self,
        budget: usize,
        starting_limit: usize,
        now: Instant,
    ) -> Result<ProbeRuntime, AdmissionError> {
        let _lifecycle = self
            .lifecycle
            .lock()
            .map_err(|_| AdmissionError::LockPoisoned)?;
        if self.closing.load(Ordering::Acquire) {
            return Err(AdmissionError::Closed);
        }
        let runtime = self
            .runtime
            .get_or_init(|| ProbeRuntime::new(budget, starting_limit, now))
            .clone();
        runtime.update_budget(budget, now)?;
        Ok(runtime)
    }

    async fn shutdown(&self) -> Result<(), AdmissionError> {
        let runtime = {
            let _lifecycle = self
                .lifecycle
                .lock()
                .map_err(|_| AdmissionError::LockPoisoned)?;
            self.closing.store(true, Ordering::Release);
            self.runtime.get().cloned()
        };
        if let Some(runtime) = runtime {
            runtime.shutdown_and_drain().await?;
        }
        Ok(())
    }
}

struct AdmissionState {
    closing: bool,
    budget: usize,
    adaptive: adaptive::AdaptiveState,
    pressure_level: PressureLevel,
    sessions: HashMap<u64, SessionEntry>,
    active_by_library: HashMap<LibraryId, usize>,
    active_attempts: HashMap<u64, Instant>,
    active_total: usize,
    epoch: u64,
    version: u64,
    last_nonempty_profile: HashMap<LibraryId, usize>,
    idle_since: Option<Instant>,
    window_started_at: Instant,
    counters: WindowCounters,
    service_samples: VecDeque<Duration>,
    saturated_since: Option<Instant>,
    saturated_for: Duration,
    last_adaptive_tick: Option<Instant>,
}

#[derive(Clone, Copy)]
struct SessionEntry {
    library_id: LibraryId,
    configured_limit: usize,
    remaining: usize,
    active: usize,
    closed: bool,
    pressure: Option<ProbePressure>,
}

#[derive(Default)]
struct WindowCounters {
    successes: u32,
    failures: u32,
    timeouts: u32,
}

impl ProbeRuntime {
    pub(super) fn new(budget: usize, starting_limit: usize, now: Instant) -> Self {
        let budget = budget.clamp(1, MAX_EFFECTIVE_PROBE_CONCURRENCY);
        let starting_limit = starting_limit.clamp(1, budget);
        Self {
            inner: Arc::new(ProbeRuntimeInner {
                state: Mutex::new(AdmissionState {
                    closing: false,
                    budget,
                    adaptive: adaptive::AdaptiveState::new(starting_limit, budget, 0, now),
                    pressure_level: PressureLevel::Normal,
                    sessions: HashMap::new(),
                    active_by_library: HashMap::new(),
                    active_attempts: HashMap::new(),
                    active_total: 0,
                    epoch: 0,
                    version: 0,
                    last_nonempty_profile: HashMap::new(),
                    idle_since: Some(now),
                    window_started_at: now,
                    counters: WindowCounters::default(),
                    service_samples: VecDeque::with_capacity(LATENCY_SAMPLE_CAPACITY),
                    saturated_since: None,
                    saturated_for: Duration::ZERO,
                    last_adaptive_tick: None,
                }),
                changed: Notify::new(),
                next_session_id: AtomicU64::new(1),
                next_attempt_id: AtomicU64::new(1),
            }),
        }
    }

    pub(super) fn global(
        budget: usize,
        starting_limit: usize,
        now: Instant,
    ) -> Result<Self, AdmissionError> {
        GLOBAL_PROBE_RUNTIME.get_or_init(budget, starting_limit, now)
    }

    pub(super) fn register(
        &self,
        library_id: LibraryId,
        configured_limit: usize,
        remaining: usize,
        now: Instant,
    ) -> Result<ProbeSession, AdmissionError> {
        let session_id = self.inner.next_session_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut state = self.lock_state()?;
            if state.closing {
                return Err(AdmissionError::Closed);
            }
            state.refresh_saturation(now);
            state.sessions.insert(
                session_id,
                SessionEntry {
                    library_id,
                    configured_limit: configured_limit.clamp(1, MAX_EFFECTIVE_PROBE_CONCURRENCY),
                    remaining,
                    active: 0,
                    closed: false,
                    pressure: None,
                },
            );
            state.refresh_profile(now);
            state.refresh_saturation(now);
            state.version = state.version.wrapping_add(1);
        }
        self.inner.changed.notify_waiters();
        Ok(ProbeSession {
            runtime: self.clone(),
            session_id,
        })
    }

    pub(super) fn update_budget(&self, budget: usize, now: Instant) -> Result<(), AdmissionError> {
        let budget = budget.clamp(1, MAX_EFFECTIVE_PROBE_CONCURRENCY);
        let adjustment = {
            let mut state = self.lock_state()?;
            if state.budget == budget {
                return Ok(());
            }
            state.refresh_saturation(now);
            state.budget = budget;
            let adjustment = state.adaptive.update_budget(budget);
            state.epoch = state.epoch.wrapping_add(1);
            let epoch = state.epoch;
            state.adaptive.reset_epoch(epoch, now);
            state.reset_window(now);
            state.refresh_saturation(now);
            state.version = state.version.wrapping_add(1);
            adjustment
        };
        if let Some((previous_limit, current_limit)) = adjustment {
            tracing::debug!(
                previous_limit,
                current_limit,
                "probe concurrency budget decreased"
            );
        }
        self.inner.changed.notify_waiters();
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn current_limit(&self) -> Result<usize, AdmissionError> {
        Ok(self.lock_state()?.adaptive.current_limit())
    }

    #[cfg(test)]
    pub(super) fn active_count(&self) -> Result<usize, AdmissionError> {
        Ok(self.lock_state()?.active_total)
    }

    #[cfg(test)]
    pub(super) fn pressure_level(&self) -> Result<PressureLevel, AdmissionError> {
        Ok(self.lock_state()?.pressure_level)
    }

    fn update_pressure(
        &self,
        session_id: u64,
        pressure: ProbePressure,
        now: Instant,
    ) -> Result<(), AdmissionError> {
        let (changed, adjustment) = {
            let mut state = self.lock_state()?;
            if state.closing {
                return Err(AdmissionError::Closed);
            }
            let Some(session) = state.sessions.get_mut(&session_id) else {
                return Err(AdmissionError::Closed);
            };
            if session.closed {
                return Err(AdmissionError::Closed);
            }
            session.pressure = Some(pressure);
            let (changed, adjustment) = state.refresh_pressure(now);
            if changed {
                state.refresh_saturation(now);
                state.version = state.version.wrapping_add(1);
            }
            (changed, adjustment)
        };
        self.log_adjustment(adjustment);
        if changed {
            self.inner.changed.notify_waiters();
        }
        Ok(())
    }

    pub(super) fn version(&self) -> Result<u64, AdmissionError> {
        Ok(self.lock_state()?.version)
    }

    pub(super) fn is_closing(&self) -> Result<bool, AdmissionError> {
        Ok(self.lock_state()?.closing)
    }

    pub(super) async fn shutdown_and_drain(&self) -> Result<(), AdmissionError> {
        self.close_admission()?;
        loop {
            let (version, drained) = {
                let state = self.lock_state()?;
                (
                    state.version,
                    state.active_total == 0 && state.sessions.is_empty(),
                )
            };
            if drained {
                return Ok(());
            }
            self.wait_for_change_since(version).await?;
        }
    }

    fn close_admission(&self) -> Result<(), AdmissionError> {
        let now = Instant::now();
        let changed = {
            let mut state = self.lock_state()?;
            if state.closing {
                false
            } else {
                state.closing = true;
                for session in state.sessions.values_mut() {
                    session.closed = true;
                    session.remaining = 0;
                }
                state.refresh_profile(now);
                let _ = state.refresh_pressure(now);
                state.refresh_saturation(now);
                state.version = state.version.wrapping_add(1);
                true
            }
        };
        if changed {
            self.inner.changed.notify_waiters();
        }
        Ok(())
    }

    pub(super) async fn wait_for_change_since(
        &self,
        observed_version: u64,
    ) -> Result<(), AdmissionError> {
        let notified = self.inner.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.version()? != observed_version {
            return Ok(());
        }
        notified.await;
        Ok(())
    }

    pub(super) fn tick(
        &self,
        now: Instant,
    ) -> Result<Option<adaptive::Adjustment>, AdmissionError> {
        let (adjustment, changed) = {
            let mut state = self.lock_state()?;
            state.refresh_saturation(now);
            let (pressure_changed, pressure_adjustment) = state.refresh_pressure(now);
            let mut adjustment = pressure_adjustment;

            let tick_due = !state
                .last_adaptive_tick
                .is_some_and(|last| now.saturating_duration_since(last) < TICK_INTERVAL);
            if tick_due {
                state.last_adaptive_tick = Some(now);

                if !pressure_changed {
                    let has_backlog = state.total_remaining() > 0;
                    let longest_active_for = state.longest_active_for(now);
                    let active_total = state.active_total;
                    adjustment = state.adaptive.evaluate_stall(
                        now,
                        has_backlog,
                        active_total,
                        longest_active_for,
                    );

                    let elapsed = now.saturating_duration_since(state.window_started_at);
                    let window_ready = elapsed >= WINDOW_MAX
                        || (elapsed >= WINDOW_MIN && state.counters.successes >= 16);
                    if window_ready {
                        state.refresh_saturation(now);
                        let input = state.window_input(now);
                        if let Some(window_adjustment) = state.adaptive.evaluate(now, input) {
                            adjustment = Some(window_adjustment);
                        }
                        state.reset_window(now);
                        state.refresh_saturation(now);
                    }
                }
            }

            let changed = pressure_changed || adjustment.is_some();
            if changed {
                state.refresh_saturation(now);
                state.version = state.version.wrapping_add(1);
            }
            (adjustment, changed)
        };
        self.log_adjustment(adjustment);
        if changed {
            self.inner.changed.notify_waiters();
        }
        Ok(adjustment)
    }

    fn log_adjustment(&self, adjustment: Option<adaptive::Adjustment>) {
        if let Some(adjustment) = adjustment {
            tracing::debug!(
                previous_limit = adjustment.previous_limit,
                current_limit = adjustment.current_limit,
                reason = ?adjustment.reason,
                "adaptively adjusted ffprobe concurrency"
            );
        }
    }

    fn try_acquire(
        &self,
        session_id: u64,
        now: Instant,
    ) -> Result<Option<ProbePermit>, AdmissionError> {
        let attempt_id = self.inner.next_attempt_id.fetch_add(1, Ordering::Relaxed);
        let permit = {
            let mut state = self.lock_state()?;
            if state.closing {
                return Err(AdmissionError::Closed);
            }
            state.refresh_saturation(now);
            let Some(session) = state.sessions.get(&session_id).copied() else {
                return Err(AdmissionError::Closed);
            };
            if session.closed || session.remaining == 0 {
                return Ok(None);
            }
            let library_limit = state.library_limit(session.library_id);
            let library_active = state
                .active_by_library
                .get(&session.library_id)
                .copied()
                .unwrap_or_default();
            if state.active_total >= state.adaptive.current_limit()
                || library_active >= library_limit
            {
                return Ok(None);
            }

            if let Some(session) = state.sessions.get_mut(&session_id) {
                session.remaining = session.remaining.saturating_sub(1);
                session.active = session.active.saturating_add(1);
            }
            state.active_total = state.active_total.saturating_add(1);
            *state
                .active_by_library
                .entry(session.library_id)
                .or_default() += 1;
            state.active_attempts.insert(attempt_id, now);
            state.refresh_profile(now);
            state.refresh_saturation(now);
            state.version = state.version.wrapping_add(1);
            ProbePermit {
                runtime: self.clone(),
                session_id,
                library_id: session.library_id,
                attempt_id,
                started_at: now,
                completed: false,
            }
        };
        self.inner.changed.notify_waiters();
        Ok(Some(permit))
    }

    fn close_session(&self, session_id: u64, now: Instant) {
        let result = self.lock_state();
        let Ok(mut state) = result else {
            tracing::error!(
                "probe session could not unregister because admission state is unavailable"
            );
            return;
        };
        state.refresh_saturation(now);
        if let Some(session) = state.sessions.get_mut(&session_id) {
            session.closed = true;
            session.remaining = 0;
            if session.active == 0 {
                state.sessions.remove(&session_id);
            }
        }
        state.refresh_profile(now);
        let _ = state.refresh_pressure(now);
        state.refresh_saturation(now);
        state.version = state.version.wrapping_add(1);
        drop(state);
        self.inner.changed.notify_waiters();
    }

    fn complete_attempt(
        &self,
        session_id: u64,
        library_id: LibraryId,
        attempt_id: u64,
        started_at: Instant,
        outcome: AttemptOutcome,
        now: Instant,
    ) -> Result<(), AdmissionError> {
        let mut state = self.lock_state()?;
        state.refresh_saturation(now);
        if state.active_attempts.remove(&attempt_id).is_none() {
            return Ok(());
        }
        state.active_total = state.active_total.saturating_sub(1);
        let remove_library = if let Some(active) = state.active_by_library.get_mut(&library_id) {
            *active = active.saturating_sub(1);
            *active == 0
        } else {
            false
        };
        if remove_library {
            state.active_by_library.remove(&library_id);
        }
        let remove_session = if let Some(session) = state.sessions.get_mut(&session_id) {
            session.active = session.active.saturating_sub(1);
            session.closed && session.active == 0
        } else {
            false
        };
        if remove_session {
            state.sessions.remove(&session_id);
        }

        let elapsed = now.saturating_duration_since(started_at);
        match outcome {
            AttemptOutcome::Success => {
                state.counters.successes = state.counters.successes.saturating_add(1);
                state.adaptive.record_success(now);
                push_sample(&mut state.service_samples, elapsed);
            }
            AttemptOutcome::Failure => {
                state.counters.failures = state.counters.failures.saturating_add(1);
                push_sample(&mut state.service_samples, elapsed);
            }
            AttemptOutcome::Timeout => {
                state.counters.timeouts = state.counters.timeouts.saturating_add(1);
                push_sample(&mut state.service_samples, elapsed);
            }
            AttemptOutcome::Cancelled => {}
        }
        state.refresh_profile(now);
        let _ = state.refresh_pressure(now);
        state.refresh_saturation(now);
        state.version = state.version.wrapping_add(1);
        drop(state);
        self.inner.changed.notify_waiters();
        Ok(())
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, AdmissionState>, AdmissionError> {
        self.inner
            .state
            .lock()
            .map_err(|_| AdmissionError::LockPoisoned)
    }
}

pub(super) fn global_runtime(
    budget: usize,
    starting_limit: usize,
    now: Instant,
) -> Result<ProbeRuntime, AdmissionError> {
    ProbeRuntime::global(budget, starting_limit, now)
}

pub(super) async fn shutdown_global_runtime() -> Result<(), AdmissionError> {
    GLOBAL_PROBE_RUNTIME.shutdown().await
}

pub(super) struct ProbeSession {
    runtime: ProbeRuntime,
    session_id: u64,
}

impl ProbeSession {
    pub(super) fn try_acquire(&self, now: Instant) -> Result<Option<ProbePermit>, AdmissionError> {
        self.runtime.try_acquire(self.session_id, now)
    }

    pub(super) fn update_pressure(
        &self,
        pressure: ProbePressure,
        now: Instant,
    ) -> Result<(), AdmissionError> {
        self.runtime.update_pressure(self.session_id, pressure, now)
    }
}

impl Drop for ProbeSession {
    fn drop(&mut self) {
        self.runtime.close_session(self.session_id, Instant::now());
    }
}

pub(super) struct ProbePermit {
    runtime: ProbeRuntime,
    session_id: u64,
    library_id: LibraryId,
    attempt_id: u64,
    started_at: Instant,
    completed: bool,
}

impl ProbePermit {
    pub(super) fn complete(mut self, outcome: AttemptOutcome) {
        self.finish(outcome);
    }

    fn finish(&mut self, outcome: AttemptOutcome) {
        if self.completed {
            return;
        }
        if let Err(error) = self.runtime.complete_attempt(
            self.session_id,
            self.library_id,
            self.attempt_id,
            self.started_at,
            outcome,
            Instant::now(),
        ) {
            tracing::error!(%error, "probe attempt permit could not update admission state");
        }
        self.completed = true;
    }
}

impl Drop for ProbePermit {
    fn drop(&mut self) {
        self.finish(AttemptOutcome::Cancelled);
    }
}

impl AdmissionState {
    fn refresh_pressure(&mut self, now: Instant) -> (bool, Option<adaptive::Adjustment>) {
        let current = self
            .sessions
            .values()
            .filter(|session| session.remaining > 0 || session.active > 0)
            .filter_map(|session| session.pressure)
            .filter(|pressure| pressure.is_fresh_at(now))
            .map(|pressure| {
                adaptive::classify_pressure(
                    pressure.home_p95_ms,
                    pressure.container_memory_usage_percent,
                )
            })
            .max()
            .unwrap_or(PressureLevel::Normal);
        if current == self.pressure_level {
            return (false, None);
        }

        let previous = self.pressure_level;
        self.pressure_level = current;
        self.epoch = self.epoch.wrapping_add(1);
        self.adaptive.reset_epoch(self.epoch, now);
        self.reset_window(now);
        let adjustment = self.adaptive.apply_pressure_change(now, previous, current);
        (true, adjustment)
    }

    fn total_remaining(&self) -> usize {
        self.sessions.values().fold(0_usize, |total, session| {
            total.saturating_add(session.remaining)
        })
    }

    fn longest_active_for(&self, now: Instant) -> Duration {
        self.active_attempts
            .values()
            .map(|started_at| now.saturating_duration_since(*started_at))
            .max()
            .unwrap_or(Duration::ZERO)
    }

    fn library_limit(&self, library_id: LibraryId) -> usize {
        self.sessions
            .values()
            .filter(|session| {
                session.library_id == library_id && (session.remaining > 0 || session.active > 0)
            })
            .map(|session| session.configured_limit)
            .min()
            .unwrap_or(MAX_EFFECTIVE_PROBE_CONCURRENCY)
    }

    fn refresh_saturation(&mut self, now: Instant) {
        if let Some(saturated_since) = self.saturated_since.take() {
            self.saturated_for = self
                .saturated_for
                .saturating_add(now.saturating_duration_since(saturated_since));
        }
        if self.active_total >= self.adaptive.current_limit() && self.total_remaining() > 0 {
            self.saturated_since = Some(now);
        }
    }

    fn refresh_profile(&mut self, now: Instant) {
        let mut profile = HashMap::<LibraryId, usize>::new();
        for session in self
            .sessions
            .values()
            .filter(|session| session.remaining > 0 || session.active > 0)
        {
            profile
                .entry(session.library_id)
                .and_modify(|limit| *limit = (*limit).min(session.configured_limit))
                .or_insert(session.configured_limit);
        }
        if profile.is_empty() {
            self.idle_since.get_or_insert(now);
            return;
        }

        let aged_idle = self.idle_since.is_some_and(|idle_since| {
            now.saturating_duration_since(idle_since) >= IDLE_EPOCH_RESET
        });
        if aged_idle || profile != self.last_nonempty_profile {
            self.epoch = self.epoch.wrapping_add(1);
            self.adaptive.reset_epoch(self.epoch, now);
            self.reset_window(now);
            self.refresh_saturation(now);
        }
        self.last_nonempty_profile = profile;
        self.idle_since = None;
    }

    fn reset_window(&mut self, now: Instant) {
        self.window_started_at = now;
        self.counters = WindowCounters::default();
        self.service_samples.clear();
        self.saturated_for = Duration::ZERO;
        self.saturated_since = None;
    }

    fn window_input(&self, now: Instant) -> adaptive::WindowInput {
        let mut samples = self.service_samples.iter().copied().collect::<Vec<_>>();
        samples.sort_unstable();
        let p95_index = (samples.len() * 95).div_ceil(100).saturating_sub(1);
        let service_time_p95 = samples.get(p95_index).copied().unwrap_or(Duration::ZERO);
        adaptive::WindowInput {
            epoch: self.epoch,
            elapsed: now.saturating_duration_since(self.window_started_at),
            successes: self.counters.successes,
            failures: self.counters.failures,
            timeouts: self.counters.timeouts,
            saturated_for: self.saturated_for,
            service_time_p95,
            has_backlog: self.total_remaining() > 0,
            active_processes: self.active_total,
            longest_active_for: self.longest_active_for(now),
        }
    }
}

fn push_sample(samples: &mut VecDeque<Duration>, value: Duration) {
    if samples.len() == LATENCY_SAMPLE_CAPACITY {
        samples.pop_front();
    }
    samples.push_back(value);
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::domain::ids::LibraryId;

    use super::super::adaptive::{AdjustmentReason, PressureLevel};
    use super::{AdmissionError, AttemptOutcome, ProbeRuntime, ProbeRuntimeRegistry};
    use crate::observability::resources::ProbePressure;

    #[test]
    fn applies_one_process_limit_and_each_library_limit_across_sessions() {
        let now = Instant::now();
        let runtime = ProbeRuntime::new(2, 2, now);
        let library_a = LibraryId::new();
        let library_b = LibraryId::new();
        let session_a = runtime.register(library_a, 1, 2, now).expect("session A");
        let session_b = runtime.register(library_b, 2, 3, now).expect("session B");

        let permit_a = session_a
            .try_acquire(now)
            .expect("admission state should be available")
            .expect("library A should get its first slot");
        assert!(
            session_a
                .try_acquire(now)
                .expect("admission state")
                .is_none()
        );
        let permit_b = session_b
            .try_acquire(now)
            .expect("admission state should be available")
            .expect("library B should use the second process slot");
        assert!(
            session_b
                .try_acquire(now)
                .expect("admission state")
                .is_none()
        );
        assert_eq!(runtime.active_count().expect("active count"), 2);

        permit_a.complete(AttemptOutcome::Success);
        let next_a = session_a
            .try_acquire(now + Duration::from_secs(1))
            .expect("admission state should be available")
            .expect("released process slot should be reusable");
        assert_eq!(runtime.active_count().expect("active count"), 2);
        next_a.complete(AttemptOutcome::Failure);
        permit_b.complete(AttemptOutcome::Success);
    }

    #[test]
    fn the_smallest_same_library_limit_remains_until_its_active_work_finishes() {
        let now = Instant::now();
        let runtime = ProbeRuntime::new(4, 4, now);
        let library = LibraryId::new();
        let small = runtime.register(library, 1, 2, now).expect("small session");
        let large = runtime.register(library, 4, 2, now).expect("large session");
        let permit = small
            .try_acquire(now)
            .expect("admission state")
            .expect("first session should acquire a slot");
        assert!(large.try_acquire(now).expect("admission state").is_none());

        drop(small);
        assert!(large.try_acquire(now).expect("admission state").is_none());
        permit.complete(AttemptOutcome::Success);
        assert!(
            large
                .try_acquire(now + Duration::from_secs(1))
                .expect("admission state")
                .is_some()
        );
    }

    #[test]
    fn lowering_the_budget_immediately_closes_future_admission() {
        let now = Instant::now();
        let runtime = ProbeRuntime::new(4, 4, now);
        let library = LibraryId::new();
        let session = runtime.register(library, 4, 4, now).expect("session");
        let permit = session
            .try_acquire(now)
            .expect("admission state")
            .expect("initial slot should be available");

        runtime
            .update_budget(1, now + Duration::from_secs(1))
            .expect("budget update");
        assert_eq!(runtime.current_limit().expect("limit"), 1);
        assert!(session.try_acquire(now).expect("state").is_none());
        permit.complete(AttemptOutcome::Success);
        assert!(session.try_acquire(now).expect("state").is_some());
    }

    #[test]
    fn stalled_backlog_lowers_future_admission_without_releasing_active_work() {
        let now = Instant::now();
        let runtime = ProbeRuntime::new(8, 8, now);
        let session = runtime
            .register(LibraryId::new(), 8, 16, now)
            .expect("session");
        let permits = (0..8)
            .map(|_| {
                session
                    .try_acquire(now)
                    .expect("admission state")
                    .expect("each initial slot should be granted")
            })
            .collect::<Vec<_>>();

        let adjustment = runtime
            .tick(now + Duration::from_secs(31))
            .expect("admission state")
            .expect("a stalled saturated queue should reduce its limit");
        assert_eq!(adjustment.reason, AdjustmentReason::Stall);
        assert_eq!(runtime.current_limit().expect("limit"), 4);
        assert_eq!(runtime.active_count().expect("active count"), 8);
        assert!(session.try_acquire(now).expect("state").is_none());
        drop(permits);
    }

    #[tokio::test]
    async fn admission_wait_registration_does_not_lose_a_release_notification() {
        let now = Instant::now();
        let runtime = ProbeRuntime::new(1, 1, now);
        let library = LibraryId::new();
        let session = runtime.register(library, 1, 2, now).expect("session");
        let permit = session
            .try_acquire(now)
            .expect("state")
            .expect("first permit");
        let version = runtime.version().expect("version");
        let waiter_runtime = runtime.clone();
        let waiter =
            tokio::spawn(async move { waiter_runtime.wait_for_change_since(version).await });
        permit.complete(AttemptOutcome::Success);
        waiter
            .await
            .expect("wait task")
            .expect("release should notify registered waiters");
        assert_eq!(runtime.active_count().expect("active count"), 0);
    }

    #[tokio::test]
    async fn shutdown_closes_admission_and_waits_for_active_permits_to_finish() {
        let now = Instant::now();
        let runtime = ProbeRuntime::new(1, 1, now);
        let session = runtime
            .register(LibraryId::new(), 1, 1, now)
            .expect("session");
        let permit = session
            .try_acquire(now)
            .expect("admission state")
            .expect("permit");
        drop(session);
        let observed_version = runtime.version().expect("version");

        let draining_runtime = runtime.clone();
        let draining = tokio::spawn(async move { draining_runtime.shutdown_and_drain().await });
        runtime
            .wait_for_change_since(observed_version)
            .await
            .expect("shutdown should wake waiters");

        assert_eq!(runtime.active_count().expect("active count"), 1);
        assert_eq!(
            runtime.register(LibraryId::new(), 1, 1, now).err(),
            Some(AdmissionError::Closed)
        );
        drop(permit);
        draining
            .await
            .expect("drain task")
            .expect("active permits should drain");
        assert_eq!(runtime.active_count().expect("active count"), 0);
        assert!(runtime.is_closing().expect("closing state"));
        runtime
            .shutdown_and_drain()
            .await
            .expect("shutdown should be idempotent");
    }

    #[tokio::test]
    async fn shutdown_waits_for_idle_page_sessions_to_drop() {
        let now = Instant::now();
        let runtime = ProbeRuntime::new(1, 1, now);
        let session = runtime
            .register(LibraryId::new(), 1, 0, now)
            .expect("sidecar-only page session");
        let observed_version = runtime.version().expect("version");

        let draining_runtime = runtime.clone();
        let draining = tokio::spawn(async move { draining_runtime.shutdown_and_drain().await });
        runtime
            .wait_for_change_since(observed_version)
            .await
            .expect("shutdown should wake waiters");

        assert!(!draining.is_finished());
        drop(session);
        draining
            .await
            .expect("drain task")
            .expect("idle sessions should drain after their page exits");
    }

    #[test]
    fn closed_runtime_rejects_new_sessions() {
        let now = Instant::now();
        let runtime = ProbeRuntime::new(1, 1, now);
        {
            let mut state = runtime.inner.state.lock().expect("state");
            state.closing = true;
        }
        assert_eq!(
            runtime.register(LibraryId::new(), 1, 1, now).err(),
            Some(AdmissionError::Closed)
        );
    }

    #[tokio::test]
    async fn registry_shutdown_prevents_late_runtime_initialization() {
        let registry = ProbeRuntimeRegistry::new();
        registry
            .shutdown()
            .await
            .expect("shutdown before initialization");

        assert!(registry.runtime.get().is_none());
        assert_eq!(
            registry.get_or_init(8, 4, Instant::now()).err(),
            Some(AdmissionError::Closed)
        );
    }

    #[test]
    fn active_sessions_aggregate_the_most_severe_fresh_pressure_and_expire_stale_samples() {
        let now = Instant::now();
        let runtime = ProbeRuntime::new(16, 8, now);
        let degraded = runtime
            .register(LibraryId::new(), 8, 1, now)
            .expect("degraded session");
        let severe = runtime
            .register(LibraryId::new(), 8, 1, now)
            .expect("severe session");

        degraded
            .update_pressure(
                ProbePressure {
                    home_p95_ms: Some(1_000),
                    container_memory_usage_percent: None,
                    sampled_at: now,
                },
                now,
            )
            .expect("update degraded pressure");
        assert_eq!(
            runtime.pressure_level().expect("pressure level"),
            PressureLevel::Degraded
        );
        assert_eq!(runtime.current_limit().expect("limit"), 4);

        severe
            .update_pressure(
                ProbePressure {
                    home_p95_ms: None,
                    container_memory_usage_percent: Some(95.0),
                    sampled_at: now,
                },
                now,
            )
            .expect("update severe pressure");
        assert_eq!(
            runtime.pressure_level().expect("pressure level"),
            PressureLevel::Severe
        );
        assert_eq!(runtime.current_limit().expect("limit"), 1);

        drop(severe);
        assert_eq!(
            runtime.pressure_level().expect("pressure level"),
            PressureLevel::Degraded
        );
        assert_eq!(runtime.current_limit().expect("limit"), 1);

        runtime
            .tick(now + Duration::from_secs(11))
            .expect("expire stale pressure");
        assert_eq!(
            runtime.pressure_level().expect("pressure level"),
            PressureLevel::Normal
        );
        assert_eq!(runtime.current_limit().expect("limit"), 1);
    }

    #[test]
    fn sessions_without_pending_or_active_work_do_not_contribute_pressure() {
        let now = Instant::now();
        let runtime = ProbeRuntime::new(8, 4, now);
        let idle = runtime
            .register(LibraryId::new(), 4, 0, now)
            .expect("idle session");
        idle.update_pressure(
            ProbePressure {
                home_p95_ms: Some(2_000),
                container_memory_usage_percent: None,
                sampled_at: now,
            },
            now,
        )
        .expect("update idle session pressure");
        assert_eq!(
            runtime.pressure_level().expect("pressure level"),
            PressureLevel::Normal
        );
        assert_eq!(runtime.current_limit().expect("limit"), 4);
    }
}
