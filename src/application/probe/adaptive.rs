use std::time::{Duration, Instant};

const MIN_WINDOW: Duration = Duration::from_secs(10);
const MAX_WINDOW: Duration = Duration::from_secs(30);
const MIN_SUCCESS_SAMPLES: u32 = 16;
const MIN_SATURATION_PERCENT: u32 = 80;
const TRIAL_GAIN_PERCENT: u32 = 5;
const TRIAL_P95_INCREASE_PERCENT: u32 = 20;
const TRIAL_TIMEOUT_INCREASE_PERCENT: u32 = 0;
const DEGRADATION_PERCENT: u32 = 15;
const TRIAL_COOLDOWN: Duration = Duration::from_secs(30);
const REVERSE_TRIAL_AFTER: Duration = Duration::from_secs(60);
const STALL_AFTER: Duration = Duration::from_secs(30);
const PROBE_HOME_P95_DEGRADED_MS: u64 = 1_000;
const PROBE_HOME_P95_SEVERE_MS: u64 = 2_000;
const PROBE_MEMORY_DEGRADED_PERCENT: f64 = 85.0;
const PROBE_MEMORY_SEVERE_PERCENT: f64 = 95.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdjustmentReason {
    TrialUp,
    TrialRejected,
    SustainedDegradation,
    Stall,
    FailureSafety,
    TrialDown,
    TrialDownRejected,
    ResourcePressure,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum PressureLevel {
    Normal,
    Degraded,
    Severe,
}

pub(super) fn classify_pressure(
    home_p95_ms: Option<u64>,
    container_memory_usage_percent: Option<f64>,
) -> PressureLevel {
    if home_p95_ms.is_some_and(|value| value >= PROBE_HOME_P95_SEVERE_MS)
        || container_memory_usage_percent.is_some_and(|value| value >= PROBE_MEMORY_SEVERE_PERCENT)
    {
        PressureLevel::Severe
    } else if home_p95_ms.is_some_and(|value| value >= PROBE_HOME_P95_DEGRADED_MS)
        || container_memory_usage_percent
            .is_some_and(|value| value >= PROBE_MEMORY_DEGRADED_PERCENT)
    {
        PressureLevel::Degraded
    } else {
        PressureLevel::Normal
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Adjustment {
    pub(super) previous_limit: usize,
    pub(super) current_limit: usize,
    pub(super) reason: AdjustmentReason,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct WindowInput {
    pub(super) epoch: u64,
    pub(super) elapsed: Duration,
    pub(super) successes: u32,
    pub(super) failures: u32,
    pub(super) timeouts: u32,
    pub(super) saturated_for: Duration,
    pub(super) service_time_p95: Duration,
    pub(super) has_backlog: bool,
    pub(super) active_processes: usize,
    pub(super) longest_active_for: Duration,
}

#[derive(Clone, Copy, Debug)]
struct WindowStats {
    throughput: f64,
    timeout_rate: f64,
    service_time_p95: Duration,
}

#[derive(Clone, Copy, Debug)]
enum AdaptivePhase {
    Warmup,
    Stable {
        baseline: WindowStats,
        since: Instant,
        degraded_windows: u8,
    },
    TrialUp {
        original_limit: usize,
        baseline: WindowStats,
        passing_windows: u8,
    },
    TrialDown {
        original_limit: usize,
        baseline: WindowStats,
    },
    Cooldown {
        until: Instant,
        baseline: Option<WindowStats>,
    },
}

#[derive(Clone, Debug)]
pub(super) struct AdaptiveState {
    starting_limit: usize,
    current_limit: usize,
    budget: usize,
    epoch: u64,
    phase: AdaptivePhase,
    last_success_at: Instant,
    last_stall_reduction_at: Option<Instant>,
    upward_hold_until: Option<Instant>,
}

impl AdaptiveState {
    pub(super) fn new(starting_limit: usize, budget: usize, epoch: u64, now: Instant) -> Self {
        let budget = budget.clamp(1, super::MAX_EFFECTIVE_PROBE_CONCURRENCY);
        let starting_limit = starting_limit.clamp(1, budget);
        Self {
            starting_limit,
            current_limit: starting_limit,
            budget,
            epoch,
            phase: AdaptivePhase::Warmup,
            last_success_at: now,
            last_stall_reduction_at: None,
            upward_hold_until: None,
        }
    }

    pub(super) fn current_limit(&self) -> usize {
        self.current_limit
    }

    pub(super) fn record_success(&mut self, now: Instant) {
        self.last_success_at = now;
    }

    pub(super) fn update_budget(&mut self, budget: usize) -> Option<(usize, usize)> {
        let budget = budget.clamp(1, super::MAX_EFFECTIVE_PROBE_CONCURRENCY);
        if self.budget == budget {
            return None;
        }
        self.budget = budget;
        self.phase = AdaptivePhase::Warmup;
        self.upward_hold_until = None;
        if self.current_limit > budget {
            let previous_limit = self.current_limit;
            self.current_limit = budget;
            Some((previous_limit, budget))
        } else {
            None
        }
    }

    pub(super) fn reset_epoch(&mut self, epoch: u64, now: Instant) {
        self.epoch = epoch;
        self.phase = AdaptivePhase::Warmup;
        self.last_success_at = now;
        self.last_stall_reduction_at = None;
        self.upward_hold_until = None;
    }

    pub(super) fn apply_pressure_change(
        &mut self,
        now: Instant,
        previous: PressureLevel,
        current: PressureLevel,
    ) -> Option<Adjustment> {
        if current <= previous {
            return None;
        }
        let divisor = match current {
            PressureLevel::Normal => return None,
            PressureLevel::Degraded => 2,
            PressureLevel::Severe => 4,
        };
        let next_limit = self.current_limit.div_ceil(divisor).max(1);
        if next_limit >= self.current_limit {
            return None;
        }
        let previous_limit = self.current_limit;
        self.current_limit = next_limit;
        self.last_success_at = now;
        self.phase = AdaptivePhase::Cooldown {
            until: now + TRIAL_COOLDOWN,
            baseline: None,
        };
        Some(Adjustment {
            previous_limit,
            current_limit: next_limit,
            reason: AdjustmentReason::ResourcePressure,
        })
    }

    pub(super) fn evaluate_stall(
        &mut self,
        now: Instant,
        has_backlog: bool,
        active_processes: usize,
        longest_active_for: Duration,
    ) -> Option<Adjustment> {
        if !has_backlog
            || active_processes == 0
            || longest_active_for < STALL_AFTER
            || now.saturating_duration_since(self.last_success_at) < STALL_AFTER
            || self
                .last_stall_reduction_at
                .is_some_and(|last| now.saturating_duration_since(last) < STALL_AFTER)
        {
            return None;
        }
        let next_limit = (self.current_limit / 2).max(1);
        if next_limit >= self.current_limit {
            return None;
        }
        let previous_limit = self.current_limit;
        self.current_limit = next_limit;
        self.last_stall_reduction_at = Some(now);
        self.phase = AdaptivePhase::Cooldown {
            until: now + TRIAL_COOLDOWN,
            baseline: None,
        };
        Some(Adjustment {
            previous_limit,
            current_limit: next_limit,
            reason: AdjustmentReason::Stall,
        })
    }

    pub(super) fn evaluate(&mut self, now: Instant, input: WindowInput) -> Option<Adjustment> {
        if input.epoch != self.epoch {
            self.epoch = input.epoch;
            self.phase = AdaptivePhase::Warmup;
            self.last_success_at = now;
            self.upward_hold_until = None;
            return None;
        }
        if input.successes > 0 {
            self.last_success_at = now;
        }

        if let Some(adjustment) = self.evaluate_stall(
            now,
            input.has_backlog,
            input.active_processes,
            input.longest_active_for,
        ) {
            return Some(adjustment);
        }

        if input.successes == 0
            && input.elapsed >= MIN_WINDOW
            && input.failures.saturating_add(input.timeouts) >= MIN_SUCCESS_SAMPLES
            && self.current_limit > self.starting_limit
        {
            let previous_limit = self.current_limit;
            self.current_limit = self.starting_limit;
            self.phase = AdaptivePhase::Cooldown {
                until: now + TRIAL_COOLDOWN,
                baseline: None,
            };
            return Some(Adjustment {
                previous_limit,
                current_limit: self.current_limit,
                reason: AdjustmentReason::FailureSafety,
            });
        }

        let stats = comparable_window(input)?;

        match self.phase {
            AdaptivePhase::Warmup => {
                self.phase = AdaptivePhase::Stable {
                    baseline: stats,
                    since: now,
                    degraded_windows: 0,
                };
                None
            }
            AdaptivePhase::Stable {
                baseline,
                since,
                degraded_windows,
            } => {
                let degraded = stats.throughput
                    <= baseline.throughput * (100 - DEGRADATION_PERCENT) as f64 / 100.0;
                let next_degraded_windows = if degraded {
                    degraded_windows.saturating_add(1)
                } else {
                    0
                };
                if next_degraded_windows >= 2 {
                    let previous_limit = self.current_limit;
                    let reduction = self.current_limit.div_ceil(4);
                    let next_limit = self.current_limit.saturating_sub(reduction).max(1);
                    if next_limit == previous_limit {
                        self.phase = AdaptivePhase::Stable {
                            baseline: stats,
                            since,
                            degraded_windows: 0,
                        };
                        return None;
                    }
                    self.current_limit = next_limit;
                    self.phase = AdaptivePhase::Cooldown {
                        until: now + TRIAL_COOLDOWN,
                        baseline: None,
                    };
                    return Some(Adjustment {
                        previous_limit,
                        current_limit: self.current_limit,
                        reason: AdjustmentReason::SustainedDegradation,
                    });
                }
                if degraded {
                    self.phase = AdaptivePhase::Stable {
                        baseline,
                        since,
                        degraded_windows: next_degraded_windows,
                    };
                    return None;
                }
                if self.current_limit > 1
                    && now.saturating_duration_since(since) >= REVERSE_TRIAL_AFTER
                {
                    let previous_limit = self.current_limit;
                    let step = self.current_limit.div_ceil(8).max(1);
                    self.current_limit = self.current_limit.saturating_sub(step).max(1);
                    self.phase = AdaptivePhase::TrialDown {
                        original_limit: previous_limit,
                        baseline: stats,
                    };
                    return Some(Adjustment {
                        previous_limit,
                        current_limit: self.current_limit,
                        reason: AdjustmentReason::TrialDown,
                    });
                }
                if self.current_limit < self.budget
                    && self.upward_hold_until.is_none_or(|until| now >= until)
                {
                    let previous_limit = self.current_limit;
                    let step = self.current_limit.div_ceil(8);
                    self.current_limit = self.current_limit.saturating_add(step).min(self.budget);
                    self.phase = AdaptivePhase::TrialUp {
                        original_limit: previous_limit,
                        baseline: stats,
                        passing_windows: 0,
                    };
                    return Some(Adjustment {
                        previous_limit,
                        current_limit: self.current_limit,
                        reason: AdjustmentReason::TrialUp,
                    });
                }
                self.phase = AdaptivePhase::Stable {
                    baseline: stats,
                    since,
                    degraded_windows: 0,
                };
                None
            }
            AdaptivePhase::TrialUp {
                original_limit,
                baseline,
                passing_windows,
            } => {
                if trial_up_is_good(stats, baseline) {
                    let passing_windows = passing_windows.saturating_add(1);
                    if passing_windows >= 2 {
                        self.phase = AdaptivePhase::Stable {
                            baseline: stats,
                            since: now,
                            degraded_windows: 0,
                        };
                    } else {
                        self.phase = AdaptivePhase::TrialUp {
                            original_limit,
                            baseline,
                            passing_windows,
                        };
                    }
                    None
                } else {
                    let previous_limit = self.current_limit;
                    self.current_limit = original_limit;
                    self.phase = AdaptivePhase::Cooldown {
                        until: now + TRIAL_COOLDOWN,
                        baseline: Some(baseline),
                    };
                    Some(Adjustment {
                        previous_limit,
                        current_limit: original_limit,
                        reason: AdjustmentReason::TrialRejected,
                    })
                }
            }
            AdaptivePhase::TrialDown {
                original_limit,
                baseline,
            } => {
                let throughput_retained = stats.throughput
                    >= baseline.throughput * (100 - TRIAL_GAIN_PERCENT) as f64 / 100.0;
                if throughput_retained {
                    self.upward_hold_until = Some(now + REVERSE_TRIAL_AFTER);
                    self.phase = AdaptivePhase::Stable {
                        baseline: stats,
                        since: now,
                        degraded_windows: 0,
                    };
                    None
                } else {
                    let previous_limit = self.current_limit;
                    self.current_limit = original_limit;
                    self.phase = AdaptivePhase::Cooldown {
                        until: now + TRIAL_COOLDOWN,
                        baseline: Some(baseline),
                    };
                    Some(Adjustment {
                        previous_limit,
                        current_limit: original_limit,
                        reason: AdjustmentReason::TrialDownRejected,
                    })
                }
            }
            AdaptivePhase::Cooldown { until, baseline } => {
                if now < until {
                    self.phase = AdaptivePhase::Cooldown { until, baseline };
                    None
                } else {
                    self.phase = AdaptivePhase::Stable {
                        baseline: baseline.unwrap_or(stats),
                        since: now,
                        degraded_windows: 0,
                    };
                    None
                }
            }
        }
    }
}

fn comparable_window(input: WindowInput) -> Option<WindowStats> {
    if input.elapsed < MIN_WINDOW
        || input.elapsed > MAX_WINDOW
        || input.successes < MIN_SUCCESS_SAMPLES
        || !input.has_backlog
        || input.elapsed.is_zero()
    {
        return None;
    }
    let saturated_percent = input
        .saturated_for
        .min(input.elapsed)
        .as_nanos()
        .saturating_mul(100)
        / input.elapsed.as_nanos();
    if saturated_percent < u128::from(MIN_SATURATION_PERCENT) {
        return None;
    }
    let classified_attempts = input
        .successes
        .saturating_add(input.failures)
        .saturating_add(input.timeouts);
    if classified_attempts == 0 {
        return None;
    }
    Some(WindowStats {
        throughput: f64::from(input.successes) / input.elapsed.as_secs_f64(),
        timeout_rate: f64::from(input.timeouts) / f64::from(classified_attempts),
        service_time_p95: input.service_time_p95,
    })
}

fn trial_up_is_good(candidate: WindowStats, baseline: WindowStats) -> bool {
    let throughput_improved =
        candidate.throughput >= baseline.throughput * (100 + TRIAL_GAIN_PERCENT) as f64 / 100.0;
    let latency_within_limit = candidate.service_time_p95.as_nanos().saturating_mul(100)
        <= baseline
            .service_time_p95
            .as_nanos()
            .saturating_mul(u128::from(100 + TRIAL_P95_INCREASE_PERCENT));
    let timeout_not_worse = candidate.timeout_rate
        <= baseline.timeout_rate * (100 + TRIAL_TIMEOUT_INCREASE_PERCENT) as f64 / 100.0;
    throughput_improved && latency_within_limit && timeout_not_worse
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{AdaptiveState, AdjustmentReason, PressureLevel, WindowInput, classify_pressure};

    fn valid_window(epoch: u64, successes: u32, p95_ms: u64) -> WindowInput {
        WindowInput {
            epoch,
            elapsed: Duration::from_secs(10),
            successes,
            failures: 0,
            timeouts: 0,
            saturated_for: Duration::from_secs(10),
            service_time_p95: Duration::from_millis(p95_ms),
            has_backlog: true,
            active_processes: 4,
            longest_active_for: Duration::from_secs(10),
        }
    }

    #[test]
    fn waits_for_a_warmup_window_before_trying_a_higher_limit() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(4, 8, 1, start);

        assert_eq!(
            state.evaluate(start + Duration::from_secs(10), valid_window(1, 20, 100)),
            None
        );
        assert_eq!(state.current_limit(), 4);

        let adjustment = state
            .evaluate(start + Duration::from_secs(20), valid_window(1, 20, 100))
            .expect("stable saturated throughput should begin a trial");
        assert_eq!(adjustment.previous_limit, 4);
        assert_eq!(adjustment.current_limit, 5);
        assert_eq!(adjustment.reason, AdjustmentReason::TrialUp);
    }

    #[test]
    fn keeps_a_higher_limit_only_after_two_good_windows() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(4, 8, 1, start);
        state.evaluate(start + Duration::from_secs(10), valid_window(1, 20, 100));
        state.evaluate(start + Duration::from_secs(20), valid_window(1, 20, 100));

        assert_eq!(
            state.evaluate(start + Duration::from_secs(30), valid_window(1, 21, 110)),
            None
        );
        assert_eq!(state.current_limit(), 5);
        assert_eq!(
            state.evaluate(start + Duration::from_secs(40), valid_window(1, 21, 110)),
            None
        );
        assert_eq!(state.current_limit(), 5);
        let next_trial = state
            .evaluate(start + Duration::from_secs(50), valid_window(1, 21, 110))
            .expect("two good trial windows should return the controller to stable mode");
        assert_eq!(next_trial.previous_limit, 5);
        assert_eq!(next_trial.current_limit, 6);
    }

    #[test]
    fn rolls_back_a_trial_without_enough_throughput_gain() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(4, 8, 1, start);
        state.evaluate(start + Duration::from_secs(10), valid_window(1, 20, 100));
        state.evaluate(start + Duration::from_secs(20), valid_window(1, 20, 100));

        let adjustment = state
            .evaluate(start + Duration::from_secs(30), valid_window(1, 20, 100))
            .expect("a flat trial should be rolled back");
        assert_eq!(adjustment.current_limit, 4);
        assert_eq!(adjustment.reason, AdjustmentReason::TrialRejected);
        assert_eq!(state.current_limit(), 4);
    }

    #[test]
    fn rolls_back_when_trial_latency_or_timeouts_worsen() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(4, 8, 1, start);
        state.evaluate(start + Duration::from_secs(10), valid_window(1, 20, 100));
        state.evaluate(start + Duration::from_secs(20), valid_window(1, 20, 100));

        let mut slow = valid_window(1, 21, 121);
        let rejected_latency = state
            .evaluate(start + Duration::from_secs(30), slow)
            .expect("p95 may grow by no more than twenty percent");
        assert_eq!(rejected_latency.reason, AdjustmentReason::TrialRejected);
        assert_eq!(state.current_limit(), 4);

        let mut state = AdaptiveState::new(4, 8, 1, start);
        state.evaluate(start + Duration::from_secs(10), valid_window(1, 20, 100));
        state.evaluate(start + Duration::from_secs(20), valid_window(1, 20, 100));
        state.evaluate(start + Duration::from_secs(30), valid_window(1, 21, 100));
        slow = valid_window(1, 21, 100);
        slow.timeouts = 1;
        let rejected_timeout = state
            .evaluate(start + Duration::from_secs(40), slow)
            .expect("trial timeouts may not worsen");
        assert_eq!(rejected_timeout.reason, AdjustmentReason::TrialRejected);
        assert_eq!(state.current_limit(), 4);
    }

    #[test]
    fn cooldown_suppresses_immediate_retries() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(4, 8, 1, start);
        state.evaluate(start + Duration::from_secs(10), valid_window(1, 20, 100));
        state.evaluate(start + Duration::from_secs(20), valid_window(1, 20, 100));
        state.evaluate(start + Duration::from_secs(30), valid_window(1, 20, 100));

        assert_eq!(
            state.evaluate(start + Duration::from_secs(40), valid_window(1, 21, 100)),
            None
        );
        assert_eq!(state.current_limit(), 4);
        assert_eq!(
            state.evaluate(start + Duration::from_secs(60), valid_window(1, 21, 100)),
            None
        );
        assert_eq!(state.current_limit(), 4);
        assert_eq!(
            state
                .evaluate(start + Duration::from_secs(70), valid_window(1, 21, 100))
                .map(|adjustment| adjustment.reason),
            Some(AdjustmentReason::TrialUp)
        );
    }

    #[test]
    fn lowers_a_stable_limit_after_two_saturated_degradation_windows() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(4, 4, 1, start);
        state.evaluate(start + Duration::from_secs(10), valid_window(1, 20, 100));
        assert_eq!(
            state.evaluate(start + Duration::from_secs(20), valid_window(1, 16, 100)),
            None
        );
        let adjustment = state
            .evaluate(start + Duration::from_secs(30), valid_window(1, 16, 100))
            .expect("two degraded saturated windows should lower the limit");
        assert_eq!(adjustment.current_limit, 3);
        assert_eq!(adjustment.reason, AdjustmentReason::SustainedDegradation);
    }

    #[test]
    fn halves_a_stalled_limit_only_when_backlogged_processes_are_running() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(8, 8, 1, start);
        let mut stalled = valid_window(1, 0, 100);
        stalled.elapsed = Duration::from_secs(30);
        stalled.successes = 0;
        stalled.saturated_for = Duration::from_secs(30);
        stalled.longest_active_for = Duration::from_secs(30);
        let adjustment = state
            .evaluate(start + Duration::from_secs(30), stalled)
            .expect("backlogged running processes with no successes should reduce admission");
        assert_eq!(adjustment.current_limit, 4);
        assert_eq!(adjustment.reason, AdjustmentReason::Stall);
    }

    #[test]
    fn throttles_repeated_stall_reductions_and_ignores_empty_demand() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(8, 8, 1, start);
        let mut stalled = valid_window(1, 0, 100);
        stalled.elapsed = Duration::from_secs(30);
        stalled.saturated_for = Duration::from_secs(30);
        stalled.longest_active_for = Duration::from_secs(30);
        assert_eq!(
            state.evaluate(start + Duration::from_secs(30), stalled),
            Some(super::Adjustment {
                previous_limit: 8,
                current_limit: 4,
                reason: AdjustmentReason::Stall,
            })
        );
        assert_eq!(
            state.evaluate(start + Duration::from_secs(45), stalled),
            None
        );
        assert_eq!(
            state
                .evaluate(start + Duration::from_secs(60), stalled)
                .map(|adjustment| adjustment.current_limit),
            Some(2)
        );

        let mut idle = AdaptiveState::new(8, 8, 1, start);
        stalled.has_backlog = false;
        assert_eq!(
            idle.evaluate(start + Duration::from_secs(30), stalled),
            None
        );
        assert_eq!(idle.current_limit(), 8);
    }

    #[test]
    fn fast_failures_return_to_the_safe_starting_limit() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(4, 8, 1, start);
        state.evaluate(start + Duration::from_secs(10), valid_window(1, 20, 100));
        state.evaluate(start + Duration::from_secs(20), valid_window(1, 20, 100));
        let mut failures = valid_window(1, 0, 100);
        failures.failures = 16;
        let adjustment = state
            .evaluate(start + Duration::from_secs(30), failures)
            .expect("rapid failures must not leave a raised limit active");
        assert_eq!(adjustment.reason, AdjustmentReason::FailureSafety);
        assert_eq!(adjustment.current_limit, 4);
    }

    #[test]
    fn retains_and_retests_a_lower_limit_after_a_successful_reverse_trial() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(4, 4, 1, start);
        state.evaluate(start + Duration::from_secs(10), valid_window(1, 20, 100));
        let lower_trial = state
            .evaluate(start + Duration::from_secs(70), valid_window(1, 20, 100))
            .expect("stable limits may test one step lower");
        assert_eq!(lower_trial.reason, AdjustmentReason::TrialDown);
        assert_eq!(state.current_limit(), 3);

        assert_eq!(
            state.evaluate(start + Duration::from_secs(80), valid_window(1, 19, 100)),
            None
        );
        assert_eq!(state.current_limit(), 3);
        assert_eq!(
            state
                .evaluate(start + Duration::from_secs(140), valid_window(1, 19, 100))
                .map(|adjustment| adjustment.reason),
            Some(AdjustmentReason::TrialDown)
        );
    }

    #[test]
    fn restores_the_original_limit_when_a_reverse_trial_loses_too_much_rate() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(4, 4, 1, start);
        state.evaluate(start + Duration::from_secs(10), valid_window(1, 20, 100));
        state.evaluate(start + Duration::from_secs(70), valid_window(1, 20, 100));
        let adjustment = state
            .evaluate(start + Duration::from_secs(80), valid_window(1, 18, 100))
            .expect("a reverse trial that loses more than five percent must roll back");
        assert_eq!(adjustment.reason, AdjustmentReason::TrialDownRejected);
        assert_eq!(adjustment.current_limit, 4);
    }

    #[test]
    fn ignores_unsaturated_windows_and_rewarms_after_an_epoch_change() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(4, 8, 1, start);
        let mut unsaturated = valid_window(1, 20, 100);
        unsaturated.saturated_for = Duration::from_secs(7);
        assert_eq!(
            state.evaluate(start + Duration::from_secs(10), unsaturated),
            None
        );
        assert_eq!(
            state.evaluate(start + Duration::from_secs(20), valid_window(2, 20, 100)),
            None
        );
        assert_eq!(state.current_limit(), 4);
    }

    #[test]
    fn waits_until_the_window_has_enough_successes_or_reaches_its_maximum() {
        let start = Instant::now();
        let mut state = AdaptiveState::new(4, 4, 1, start);
        state.evaluate(start + Duration::from_secs(10), valid_window(1, 20, 100));

        let mut short_sample = valid_window(1, 15, 100);
        short_sample.elapsed = Duration::from_secs(30);
        short_sample.saturated_for = Duration::from_secs(30);
        assert_eq!(
            state.evaluate(start + Duration::from_secs(40), short_sample),
            None
        );
        assert_eq!(state.current_limit(), 4);
    }

    #[test]
    fn resource_pressure_classifies_and_downshifts_only_on_pressure_increases() {
        let now = Instant::now();
        assert_eq!(classify_pressure(None, None), PressureLevel::Normal);
        assert_eq!(
            classify_pressure(Some(1_000), None),
            PressureLevel::Degraded
        );
        assert_eq!(classify_pressure(None, Some(95.0)), PressureLevel::Severe);

        let mut state = AdaptiveState::new(8, 16, 1, now);
        let degraded = state
            .apply_pressure_change(now, PressureLevel::Normal, PressureLevel::Degraded)
            .expect("degraded frontend or memory pressure should reduce admission");
        assert_eq!(degraded.reason, AdjustmentReason::ResourcePressure);
        assert_eq!(degraded.current_limit, 4);
        assert_eq!(
            state.apply_pressure_change(
                now + Duration::from_secs(1),
                PressureLevel::Degraded,
                PressureLevel::Degraded,
            ),
            None
        );

        let severe = state
            .apply_pressure_change(
                now + Duration::from_secs(2),
                PressureLevel::Degraded,
                PressureLevel::Severe,
            )
            .expect("severe pressure should reduce admission further");
        assert_eq!(severe.current_limit, 1);
        assert_eq!(
            state.apply_pressure_change(
                now + Duration::from_secs(3),
                PressureLevel::Severe,
                PressureLevel::Normal,
            ),
            None
        );
    }
}
