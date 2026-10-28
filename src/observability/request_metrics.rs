//! Bounded, in-process request metrics grouped by route template.
//!
//! The administrator performance endpoint reads this snapshot to answer "which route is slow"
//! without storing request paths, query strings, identifiers, or user details. Samples and route
//! keys are both capped so a hostile or misconfigured client cannot grow the process memory.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

/// Number of duration samples kept per route for percentile estimates.
const SAMPLE_CAPACITY: usize = 512;
/// Number of distinct route templates tracked before folding into the overflow bucket.
const MAX_ROUTES: usize = 256;
/// Longest route template kept verbatim; longer values are folded into the overflow bucket.
const MAX_ROUTE_LENGTH: usize = 200;
const OVERFLOW_METHOD: &str = "OTHER";
const OVERFLOW_ROUTE: &str = "other";

#[derive(Clone, Default)]
pub(crate) struct RequestMetrics {
    inner: Arc<Mutex<MetricsState>>,
}

#[derive(Default)]
struct MetricsState {
    started_at: Option<u64>,
    total_requests: u64,
    routes: HashMap<RouteKey, RouteMetrics>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct RouteKey {
    method: String,
    route: String,
}

#[derive(Default)]
struct RouteMetrics {
    requests: u64,
    errors: u64,
    max_us: u64,
    last_seen_at: Option<u64>,
    samples_us: VecDeque<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestMetricsSnapshot {
    pub started_at: Option<u64>,
    pub generated_at: u64,
    pub total_requests: u64,
    pub tracked_routes: usize,
    pub routes: Vec<RouteSnapshot>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteSnapshot {
    pub method: String,
    pub route: String,
    pub requests: u64,
    pub errors: u64,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub avg_ms: f64,
    pub max_ms: f64,
    pub last_seen_seconds_ago: u64,
}

impl RequestMetrics {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Records one completed request. `status >= 500` counts as an error; 4xx is expected traffic.
    pub(crate) fn record(&self, method: &str, route: &str, status: u16, duration: Duration) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        let now = unix_now();
        state.started_at.get_or_insert(now);
        state.total_requests = state.total_requests.saturating_add(1);

        let mut key = RouteKey {
            method: normalize_method(method),
            route: normalize_route(route),
        };
        if !state.routes.contains_key(&key) && state.routes.len() >= MAX_ROUTES {
            key = RouteKey {
                method: OVERFLOW_METHOD.to_owned(),
                route: OVERFLOW_ROUTE.to_owned(),
            };
        }

        let duration_us = u64::try_from(duration.as_micros()).unwrap_or(u64::MAX);
        let metrics = state.routes.entry(key).or_default();
        metrics.requests = metrics.requests.saturating_add(1);
        if status >= 500 {
            metrics.errors = metrics.errors.saturating_add(1);
        }
        metrics.max_us = metrics.max_us.max(duration_us);
        metrics.last_seen_at = Some(now);
        if metrics.samples_us.len() == SAMPLE_CAPACITY {
            metrics.samples_us.pop_front();
        }
        metrics.samples_us.push_back(duration_us);
    }

    /// p95 latency (microseconds) across the recorded samples of every route whose template
    /// contains `route_needle`, or `None` when no matching route has samples yet.
    ///
    /// Read-only and cheap: the background scheduler uses it to notice that foreground requests are
    /// currently slow without touching the snapshot structure the admin endpoint serves.
    pub(crate) fn route_p95_us(&self, route_needle: &str) -> Option<u64> {
        let state = self.inner.lock().ok()?;
        let mut samples: Vec<u64> = Vec::new();
        for (key, metrics) in &state.routes {
            if !key.route.contains(route_needle) {
                continue;
            }
            samples.extend(metrics.samples_us.iter().copied());
        }
        if samples.is_empty() {
            return None;
        }
        samples.sort_unstable();
        let index = samples
            .len()
            .saturating_mul(95)
            .div_ceil(100)
            .saturating_sub(1)
            .min(samples.len() - 1);
        Some(samples[index])
    }

    pub(crate) fn snapshot(&self) -> RequestMetricsSnapshot {
        let now = unix_now();
        let Ok(state) = self.inner.lock() else {
            return RequestMetricsSnapshot {
                started_at: None,
                generated_at: now,
                total_requests: 0,
                tracked_routes: 0,
                routes: Vec::new(),
            };
        };
        let mut routes = state
            .routes
            .iter()
            .map(|(key, metrics)| route_snapshot(key, metrics, now))
            .collect::<Vec<_>>();
        routes.sort_by(|left, right| {
            right
                .requests
                .cmp(&left.requests)
                .then_with(|| left.route.cmp(&right.route))
                .then_with(|| left.method.cmp(&right.method))
        });
        RequestMetricsSnapshot {
            started_at: state.started_at,
            generated_at: now,
            total_requests: state.total_requests,
            tracked_routes: routes.len(),
            routes,
        }
    }
}

fn route_snapshot(key: &RouteKey, metrics: &RouteMetrics, now: u64) -> RouteSnapshot {
    let mut samples = metrics.samples_us.iter().copied().collect::<Vec<_>>();
    samples.sort_unstable();
    let avg_ms = if samples.is_empty() {
        0.0
    } else {
        let total = samples
            .iter()
            .fold(0_u128, |total, value| total + u128::from(*value));
        total as f64 / samples.len() as f64 / 1000.0
    };
    RouteSnapshot {
        method: key.method.clone(),
        route: key.route.clone(),
        requests: metrics.requests,
        errors: metrics.errors,
        p50_ms: percentile_ms(&samples, 50),
        p90_ms: percentile_ms(&samples, 90),
        p95_ms: percentile_ms(&samples, 95),
        p99_ms: percentile_ms(&samples, 99),
        avg_ms,
        max_ms: metrics.max_us as f64 / 1000.0,
        last_seen_seconds_ago: metrics
            .last_seen_at
            .map_or(0, |last_seen| now.saturating_sub(last_seen)),
    }
}

/// Nearest-rank percentile over an ascending sample list.
fn percentile_ms(sorted_us: &[u64], percentile: usize) -> f64 {
    if sorted_us.is_empty() {
        return 0.0;
    }
    let numerator = percentile.clamp(1, 100) * sorted_us.len();
    let rank = numerator.div_ceil(100).max(1).min(sorted_us.len());
    sorted_us[rank - 1] as f64 / 1000.0
}

fn normalize_method(method: &str) -> String {
    if method.is_empty() || method.len() > 16 {
        OVERFLOW_METHOD.to_owned()
    } else {
        method.to_ascii_uppercase()
    }
}

fn normalize_route(route: &str) -> String {
    if route.is_empty() || route.len() > MAX_ROUTE_LENGTH {
        OVERFLOW_ROUTE.to_owned()
    } else {
        route.to_owned()
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record_ms(metrics: &RequestMetrics, route: &str, ms: u64) {
        metrics.record("GET", route, 200, Duration::from_millis(ms));
    }

    #[test]
    fn route_p95_us_reports_only_matching_routes() {
        let metrics = RequestMetrics::new();
        assert_eq!(metrics.route_p95_us("/api/v1/home"), None);
        for value in 1..=100 {
            record_ms(&metrics, "/api/v1/home", value);
        }
        record_ms(&metrics, "/api/v1/libraries/:id/items", 900);
        let p95 = metrics
            .route_p95_us("/api/v1/home")
            .expect("home samples are recorded");
        assert_eq!(p95, 95_000, "p95 over the home samples only");
        assert_eq!(metrics.route_p95_us("/api/v1/unknown"), None);
    }

    #[test]
    fn percentiles_use_nearest_rank() {
        let metrics = RequestMetrics::new();
        for value in 1..=100 {
            record_ms(&metrics, "/api/v1/home", value);
        }
        let snapshot = metrics.snapshot();
        let route = &snapshot.routes[0];
        assert_eq!(route.requests, 100);
        assert!((route.p50_ms - 50.0).abs() < f64::EPSILON);
        assert!((route.p90_ms - 90.0).abs() < f64::EPSILON);
        assert!((route.p95_ms - 95.0).abs() < f64::EPSILON);
        assert!((route.p99_ms - 99.0).abs() < f64::EPSILON);
        assert!((route.max_ms - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn sampled_average_covers_only_the_retained_window() {
        let metrics = RequestMetrics::new();
        for value in 1..=600 {
            record_ms(&metrics, "/api/v1/home", value);
        }
        let snapshot = metrics.snapshot();
        let route = &snapshot.routes[0];
        assert_eq!(route.requests, 600);
        // Retained window is the last 512 samples: 89..=600, average 344.5.
        assert!((route.avg_ms - 344.5).abs() < 0.001);
    }

    #[test]
    fn route_capacity_folds_into_overflow_bucket() {
        let metrics = RequestMetrics::new();
        for index in 0..(MAX_ROUTES + 20) {
            record_ms(&metrics, &format!("/api/v1/test/{index}"), 1);
        }
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.total_requests, (MAX_ROUTES + 20) as u64);
        assert!(snapshot.tracked_routes <= MAX_ROUTES + 1);
        let overflow = snapshot
            .routes
            .iter()
            .find(|route| route.route == OVERFLOW_ROUTE)
            .expect("overflow bucket");
        assert!(overflow.requests > 0);
    }

    #[test]
    fn only_server_errors_count_as_errors() {
        let metrics = RequestMetrics::new();
        metrics.record(
            "GET",
            "/api/v1/items/{item_id}",
            404,
            Duration::from_millis(2),
        );
        metrics.record(
            "GET",
            "/api/v1/items/{item_id}",
            500,
            Duration::from_millis(3),
        );
        let snapshot = metrics.snapshot();
        let route = &snapshot.routes[0];
        assert_eq!(route.requests, 2);
        assert_eq!(route.errors, 1);
    }

    #[test]
    fn oversized_route_templates_are_folded() {
        let metrics = RequestMetrics::new();
        let oversized = "x".repeat(MAX_ROUTE_LENGTH + 1);
        metrics.record("GET", &oversized, 200, Duration::from_millis(1));
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.routes[0].route, OVERFLOW_ROUTE);
    }

    #[test]
    fn snapshot_serializes_with_camel_case_fields() {
        let metrics = RequestMetrics::new();
        record_ms(&metrics, "/api/v1/home", 4);
        let value = serde_json::to_value(metrics.snapshot()).expect("snapshot JSON");
        assert!(value.get("totalRequests").is_some());
        assert!(value["routes"].is_array());
        assert!(value["routes"][0].get("p95Ms").is_some());
    }
}
