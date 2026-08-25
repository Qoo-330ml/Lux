use std::{
    collections::VecDeque,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::{fs, process::Command};

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const MEDIA_PATH: &str = "/media";
const HOME_LATENCY_SAMPLE_CAPACITY: usize = 64;
const HOME_P95_DEGRADED_MS: u64 = 300;
const HOME_P95_TARGET_MS: u64 = 400;
const PROBE_RECOVERY_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct ResourceMetrics {
    started_at: Instant,
    cpu_sample: Arc<Mutex<Option<CpuSample>>>,
    home_latency_ms: Arc<Mutex<VecDeque<u64>>>,
    probe_state: Arc<Mutex<ProbeConcurrencyState>>,
}

impl Default for ResourceMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceMetrics {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            cpu_sample: Arc::new(Mutex::new(None)),
            home_latency_ms: Arc::new(Mutex::new(VecDeque::with_capacity(
                HOME_LATENCY_SAMPLE_CAPACITY,
            ))),
            probe_state: Arc::new(Mutex::new(ProbeConcurrencyState::default())),
        }
    }

    pub fn record_home_latency(&self, duration: Duration) {
        let Ok(mut samples) = self.home_latency_ms.lock() else {
            return;
        };
        let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        if samples.len() == HOME_LATENCY_SAMPLE_CAPACITY {
            samples.pop_front();
        }
        samples.push_back(duration_ms);
    }

    pub fn home_latency_p95_ms(&self) -> Option<u64> {
        let Ok(samples) = self.home_latency_ms.lock() else {
            return None;
        };
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.iter().copied().collect::<Vec<_>>();
        sorted.sort_unstable();
        let index = (sorted.len() * 95).div_ceil(100).saturating_sub(1);
        sorted.get(index).copied()
    }

    pub async fn cpu_limit_cores(&self) -> Option<f64> {
        read_cpu_usage().await.and_then(|(_, limit)| limit)
    }

    pub async fn background_concurrency(&self, configured: usize) -> usize {
        let available_parallelism = std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1);
        let (cpu_limit, memory) = tokio::join!(self.cpu_limit_cores(), memory_snapshot());
        recommended_background_concurrency(
            configured,
            available_parallelism,
            self.home_latency_p95_ms(),
            cpu_limit,
            memory.usage_percent,
        )
    }

    pub async fn probe_concurrency(&self, configured: usize, hard_cap: usize) -> usize {
        let available_parallelism = std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1);
        let (cpu, memory) = tokio::join!(
            cpu_snapshot(Arc::clone(&self.cpu_sample)),
            memory_snapshot(),
        );
        let target = recommended_probe_concurrency(
            configured,
            available_parallelism,
            self.home_latency_p95_ms(),
            cpu.limit_cores,
            memory.usage_percent,
            cpu.usage_percent,
            hard_cap,
        );
        let Ok(mut state) = self.probe_state.lock() else {
            return target;
        };
        stabilize_probe_concurrency(
            &mut state,
            target,
            hard_cap,
            Instant::now(),
            PROBE_RECOVERY_COOLDOWN,
        )
    }

    pub async fn snapshot(&self) -> ResourceSnapshot {
        let (cpu, memory, media_storage) = tokio::join!(
            cpu_snapshot(Arc::clone(&self.cpu_sample)),
            memory_snapshot(),
            media_storage_snapshot(),
        );
        ResourceSnapshot {
            runtime_seconds: self.started_at.elapsed().as_secs(),
            cpu,
            memory,
            media_storage,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ResourceSnapshot {
    pub runtime_seconds: u64,
    pub cpu: CpuSnapshot,
    pub memory: MemorySnapshot,
    #[serde(rename = "mediaStorage")]
    pub media_storage: MediaStorageSnapshot,
}

#[derive(Debug, Serialize)]
pub struct CpuSnapshot {
    pub available: bool,
    pub source: &'static str,
    #[serde(rename = "usageCores")]
    pub usage_cores: Option<f64>,
    #[serde(rename = "capacityCores")]
    pub capacity_cores: Option<f64>,
    #[serde(rename = "usagePercent")]
    pub usage_percent: Option<f64>,
    #[serde(rename = "limitCores")]
    pub limit_cores: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct MemorySnapshot {
    pub available: bool,
    pub source: &'static str,
    #[serde(rename = "usedBytes")]
    pub used_bytes: Option<u64>,
    #[serde(rename = "limitBytes")]
    pub limit_bytes: Option<u64>,
    #[serde(rename = "usagePercent")]
    pub usage_percent: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct MediaStorageSnapshot {
    pub available: bool,
    pub source: &'static str,
    pub path: &'static str,
    #[serde(rename = "totalBytes")]
    pub total_bytes: Option<u64>,
    #[serde(rename = "usedBytes")]
    pub used_bytes: Option<u64>,
    #[serde(rename = "availableBytes")]
    pub available_bytes: Option<u64>,
    #[serde(rename = "usagePercent")]
    pub usage_percent: Option<f64>,
}

#[derive(Clone, Copy)]
struct CpuSample {
    usage_usec: u64,
    observed_at: Instant,
}

async fn cpu_snapshot(previous_sample: Arc<Mutex<Option<CpuSample>>>) -> CpuSnapshot {
    let Some((usage_usec, limit_cores)) = read_cpu_usage().await else {
        return CpuSnapshot {
            available: false,
            source: "cgroup",
            usage_cores: None,
            capacity_cores: None,
            usage_percent: None,
            limit_cores: None,
        };
    };

    let capacity_cores = select_cpu_capacity(
        limit_cores,
        std::thread::available_parallelism()
            .ok()
            .map(|value| value.get()),
    );
    let observed_at = Instant::now();
    let previous = previous_sample.lock().ok().and_then(|mut sample| {
        sample.replace(CpuSample {
            usage_usec,
            observed_at,
        })
    });
    let usage_cores = previous.and_then(|sample| {
        let elapsed_usec = observed_at.checked_duration_since(sample.observed_at)?;
        let elapsed_usec = u64::try_from(elapsed_usec.as_micros()).ok()?;
        calculate_cpu_usage_cores(usage_usec.saturating_sub(sample.usage_usec), elapsed_usec)
    });
    let usage_percent = previous.and_then(|sample| {
        let elapsed_usec = observed_at.checked_duration_since(sample.observed_at)?;
        let elapsed_usec = u64::try_from(elapsed_usec.as_micros()).ok()?;
        let capacity = capacity_cores.filter(|capacity| *capacity > 0.0)?;
        calculate_cpu_usage_percent(
            usage_usec.saturating_sub(sample.usage_usec),
            elapsed_usec,
            capacity,
        )
        .map(|percent| percent.min(100.0))
    });

    CpuSnapshot {
        available: true,
        source: "cgroup",
        usage_cores,
        capacity_cores,
        usage_percent,
        limit_cores,
    }
}

async fn read_cpu_usage() -> Option<(u64, Option<f64>)> {
    if let Some(stat) = read_cgroup_v2_file("cpu.stat").await {
        let usage_usec = parse_keyed_value(&stat, "usage_usec")?;
        let max = read_cgroup_v2_file("cpu.max").await?;
        return Some((usage_usec, parse_cpu_limit(&max)));
    }

    let usage_ns = read_cgroup_v1_file("cpu,cpuacct", "cpuacct.usage")
        .await
        .or(read_cgroup_v1_file("cpuacct", "cpuacct.usage").await)
        .and_then(|value| parse_unsigned(&value))?;
    let quota = read_cgroup_v1_file("cpu,cpuacct", "cpu.cfs_quota_us")
        .await
        .or(read_cgroup_v1_file("cpu", "cpu.cfs_quota_us").await);
    let period = read_cgroup_v1_file("cpu,cpuacct", "cpu.cfs_period_us")
        .await
        .or(read_cgroup_v1_file("cpu", "cpu.cfs_period_us").await);
    let limit_cores = quota
        .zip(period)
        .and_then(|(quota, period)| parse_cpu_quota(&quota, &period));
    Some((usage_ns / 1_000, limit_cores))
}

async fn memory_snapshot() -> MemorySnapshot {
    let (current, limit) = if let Some(current) = read_cgroup_v2_file("memory.current").await {
        let current = parse_unsigned(&current);
        let limit = read_cgroup_v2_file("memory.max")
            .await
            .and_then(|value| parse_cgroup_limit(&value));
        (current, limit)
    } else {
        let current = read_cgroup_v1_file("memory", "memory.usage_in_bytes")
            .await
            .and_then(|value| parse_unsigned(&value));
        let limit = read_cgroup_v1_file("memory", "memory.limit_in_bytes")
            .await
            .and_then(|value| parse_cgroup_limit(&value));
        (current, limit)
    };

    MemorySnapshot {
        available: current.is_some(),
        source: "cgroup",
        used_bytes: current,
        limit_bytes: limit,
        usage_percent: current.and_then(|used| memory_usage_percent(used, limit)),
    }
}

async fn media_storage_snapshot() -> MediaStorageSnapshot {
    let output = Command::new("df")
        .args(["-P", "-k", MEDIA_PATH])
        .output()
        .await;
    let Some(output) = output.ok().filter(|output| output.status.success()) else {
        return unavailable_media_storage();
    };
    let Ok(stdout) = String::from_utf8(output.stdout) else {
        return unavailable_media_storage();
    };
    let Some((total_bytes, used_bytes, available_bytes)) = parse_media_storage_values(&stdout)
    else {
        return unavailable_media_storage();
    };

    MediaStorageSnapshot {
        available: true,
        source: "container-filesystem",
        path: MEDIA_PATH,
        total_bytes: Some(total_bytes),
        used_bytes: Some(used_bytes),
        available_bytes: Some(available_bytes),
        usage_percent: calculate_storage_usage_percent(used_bytes, total_bytes),
    }
}

fn unavailable_media_storage() -> MediaStorageSnapshot {
    MediaStorageSnapshot {
        available: false,
        source: "container-filesystem",
        path: MEDIA_PATH,
        total_bytes: None,
        used_bytes: None,
        available_bytes: None,
        usage_percent: None,
    }
}

async fn read_cgroup_v2_file(name: &str) -> Option<String> {
    let relative = cgroup_relative_path().await?;
    let root = Path::new(CGROUP_ROOT);
    let mut paths = vec![root.join(&relative).join(name)];
    if !relative.as_os_str().is_empty() {
        paths.push(root.join(name));
    }
    read_current_cgroup_file(paths).await
}

async fn read_cgroup_v1_file(controller: &str, name: &str) -> Option<String> {
    let relative = cgroup_relative_path().await?;
    let root = Path::new(CGROUP_ROOT).join(controller);
    let mut paths = vec![root.join(&relative).join(name)];
    if !relative.as_os_str().is_empty() {
        paths.push(root.join(name));
    }
    read_current_cgroup_file(paths).await
}

async fn read_current_cgroup_file(paths: Vec<PathBuf>) -> Option<String> {
    let pid = std::process::id();
    for path in paths {
        let Some(parent) = path.parent() else {
            continue;
        };
        let Ok(processes) = fs::read_to_string(parent.join("cgroup.procs")).await else {
            continue;
        };
        if !process_is_member(&processes, pid) {
            continue;
        }
        if let Ok(value) = fs::read_to_string(path).await {
            return Some(value);
        }
    }
    None
}

async fn cgroup_relative_path() -> Option<PathBuf> {
    let content = fs::read_to_string("/proc/self/cgroup").await.ok()?;
    let value = content.lines().find_map(|line| {
        line.strip_prefix("0::")
            .or_else(|| line.splitn(3, ':').nth(2))
    })?;
    let path = Path::new(value);
    let relative = path.strip_prefix("/").ok()?.to_path_buf();
    if relative.components().any(|component| {
        matches!(
            component,
            Component::CurDir | Component::ParentDir | Component::RootDir
        )
    }) {
        return None;
    }
    Some(relative)
}

fn parse_keyed_value(input: &str, key: &str) -> Option<u64> {
    input.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let current_key = fields.next()?;
        let value = fields.next()?;
        (current_key == key).then(|| parse_unsigned(value))?
    })
}

fn parse_unsigned(input: &str) -> Option<u64> {
    input.trim().parse().ok()
}

fn process_is_member(processes: &str, pid: u32) -> bool {
    processes
        .lines()
        .filter_map(|line| line.trim().parse::<u32>().ok())
        .any(|candidate| candidate == pid)
}

fn parse_cgroup_limit(input: &str) -> Option<u64> {
    let value = input.split_whitespace().next()?;
    (value != "max").then(|| value.parse().ok())?
}

fn parse_cpu_limit(input: &str) -> Option<f64> {
    let mut fields = input.split_whitespace();
    let quota = fields.next()?;
    let period = fields.next()?;
    if quota == "max" {
        return None;
    }
    parse_cpu_quota(quota, period)
}

fn parse_cpu_quota(quota: &str, period: &str) -> Option<f64> {
    let quota = quota.parse::<f64>().ok()?;
    let period = period.parse::<f64>().ok()?;
    (quota > 0.0 && period > 0.0).then_some(quota / period)
}

fn parse_blocks(input: &str) -> Option<u64> {
    input.parse::<u64>().ok()?.checked_mul(1024)
}

fn parse_media_storage_values(output: &str) -> Option<(u64, u64, u64)> {
    let line = output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty() && !line.trim_start().starts_with("Filesystem"))?;
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 6 {
        return None;
    }
    let data = &fields[fields.len() - 5..fields.len() - 1];
    Some((
        parse_blocks(data[0])?,
        parse_blocks(data[1])?,
        parse_blocks(data[2])?,
    ))
}

fn calculate_cpu_usage_percent(
    delta_usage_usec: u64,
    elapsed_usec: u64,
    capacity_cores: f64,
) -> Option<f64> {
    if elapsed_usec == 0 || capacity_cores <= 0.0 {
        return None;
    }
    Some(delta_usage_usec as f64 * 100.0 / elapsed_usec as f64 / capacity_cores)
}

fn calculate_cpu_usage_cores(delta_usage_usec: u64, elapsed_usec: u64) -> Option<f64> {
    if elapsed_usec == 0 {
        return None;
    }
    Some(delta_usage_usec as f64 / elapsed_usec as f64)
}

fn select_cpu_capacity(limit_cores: Option<f64>, visible_cores: Option<usize>) -> Option<f64> {
    limit_cores.or_else(|| visible_cores.map(|cores| cores as f64))
}

fn memory_usage_percent(used_bytes: u64, limit_bytes: Option<u64>) -> Option<f64> {
    let limit_bytes = limit_bytes.filter(|limit| *limit > 0)?;
    Some((used_bytes as f64 * 100.0 / limit_bytes as f64).min(100.0))
}

fn calculate_storage_usage_percent(used_bytes: u64, total_bytes: u64) -> Option<f64> {
    (total_bytes > 0).then(|| (used_bytes as f64 * 100.0 / total_bytes as f64).min(100.0))
}

pub fn recommended_background_concurrency(
    configured: usize,
    available_parallelism: usize,
    home_p95_ms: Option<u64>,
    container_cpu_limit: Option<f64>,
    container_memory_usage_percent: Option<f64>,
) -> usize {
    let configured = configured.max(1);
    let container_parallelism = container_cpu_limit
        .filter(|limit| limit.is_finite() && *limit > 0.0)
        .map_or(available_parallelism, |limit| {
            limit.ceil().min(usize::MAX as f64) as usize
        });
    let cpu_cap = container_parallelism.saturating_sub(1).max(1);
    let base = configured.min(cpu_cap);
    let latency_adjusted = match home_p95_ms {
        Some(value) if value >= HOME_P95_TARGET_MS => 1,
        Some(value) if value >= HOME_P95_DEGRADED_MS => base.div_ceil(2).max(1),
        _ => base,
    };
    match container_memory_usage_percent {
        Some(value) if value >= 85.0 => 1,
        Some(value) if value >= 70.0 => latency_adjusted.div_ceil(2).max(1),
        _ => latency_adjusted,
    }
}

#[derive(Default)]
struct ProbeConcurrencyState {
    effective: Option<usize>,
    last_change_at: Option<Instant>,
}

fn stabilize_probe_concurrency(
    state: &mut ProbeConcurrencyState,
    target: usize,
    hard_cap: usize,
    now: Instant,
    recovery_cooldown: Duration,
) -> usize {
    let target = target.clamp(1, hard_cap.max(1));
    let Some(current) = state.effective else {
        state.effective = Some(target);
        return target;
    };
    if target < current {
        state.effective = Some(target);
        state.last_change_at = Some(now);
        return target;
    }
    if target == current {
        return current;
    }
    if state
        .last_change_at
        .is_some_and(|last_change| now.duration_since(last_change) < recovery_cooldown)
    {
        return current;
    }
    let next = current
        .saturating_mul(2)
        .max(current.saturating_add(1))
        .min(target);
    state.effective = Some(next);
    state.last_change_at = (next < target).then_some(now);
    next
}

pub fn recommended_probe_concurrency(
    configured: usize,
    available_parallelism: usize,
    home_p95_ms: Option<u64>,
    container_cpu_limit: Option<f64>,
    container_memory_usage_percent: Option<f64>,
    cpu_usage_percent: Option<f64>,
    hard_cap: usize,
) -> usize {
    let hard_cap = hard_cap.max(1);
    let configured = configured.clamp(1, hard_cap);
    let container_parallelism = container_cpu_limit
        .filter(|limit| limit.is_finite() && *limit > 0.0)
        .map_or(available_parallelism, |limit| {
            limit.ceil().min(usize::MAX as f64) as usize
        })
        .max(1);
    // ffprobe spends a meaningful amount of time waiting on media storage, so
    // it gets an I/O-oriented cap instead of reserving one worker per CPU.
    let io_cap = container_parallelism.saturating_mul(16).clamp(1, hard_cap);
    let base = configured.min(io_cap);
    let severe_pressure = cpu_usage_percent.is_some_and(|value| value >= 90.0)
        || container_memory_usage_percent.is_some_and(|value| value >= 95.0)
        || home_p95_ms.is_some_and(|value| value >= 2_000);
    if severe_pressure {
        return base.div_ceil(4).max(1);
    }
    let degraded = cpu_usage_percent.is_some_and(|value| value >= 75.0)
        || container_memory_usage_percent.is_some_and(|value| value >= 85.0)
        || home_p95_ms.is_some_and(|value| value >= 1_000);
    if degraded {
        return base.div_ceil(2).max(1);
    }
    base.max(1)
}

#[cfg(test)]
mod tests {
    use super::{
        ResourceMetrics, calculate_cpu_usage_cores, calculate_cpu_usage_percent,
        memory_usage_percent, parse_cgroup_limit, parse_media_storage_values, process_is_member,
        recommended_background_concurrency, recommended_probe_concurrency, select_cpu_capacity,
    };
    use std::time::Duration;

    #[test]
    fn cgroup_max_is_an_unlimited_container_limit() {
        assert_eq!(parse_cgroup_limit("max\n"), None);
        assert_eq!(parse_cgroup_limit("1048576\n"), Some(1_048_576));
    }

    #[test]
    fn cpu_usage_is_normalized_to_the_container_capacity() {
        assert_eq!(
            calculate_cpu_usage_percent(200_000, 1_000_000, 2.0),
            Some(10.0)
        );
        assert_eq!(
            calculate_cpu_usage_percent(2_000_000, 1_000_000, 2.0),
            Some(100.0)
        );
    }

    #[test]
    fn cpu_usage_reports_consumed_cores_for_dashboard_display() {
        assert_eq!(calculate_cpu_usage_cores(1_800_000, 1_000_000), Some(1.8));
    }

    #[test]
    fn cpu_capacity_prefers_a_cgroup_limit_over_visible_cores() {
        assert_eq!(select_cpu_capacity(Some(2.0), Some(8)), Some(2.0));
        assert_eq!(select_cpu_capacity(None, Some(8)), Some(8.0));
        assert_eq!(select_cpu_capacity(None, None), None);
    }

    #[test]
    fn memory_usage_is_unavailable_without_a_finite_container_limit() {
        assert_eq!(memory_usage_percent(512, None), None);
        assert_eq!(memory_usage_percent(512, Some(1_024)), Some(50.0));
    }

    #[test]
    fn cgroup_membership_requires_an_exact_process_id() {
        assert!(process_is_member("100\n123\n", 123));
        assert!(!process_is_member("1230\n", 123));
    }

    #[test]
    fn parses_posix_df_output_without_trusting_the_mount_label() {
        assert_eq!(
            parse_media_storage_values(
                "Filesystem 1024-blocks Used Available Capacity Mounted on\n                 overlay 102400 20480 81920 20% /media\n"
            ),
            Some((102_400 * 1024, 20_480 * 1024, 81_920 * 1024))
        );
    }

    #[test]
    fn home_latency_p95_keeps_only_the_recent_window() {
        let metrics = ResourceMetrics::new();
        for value in 1..=65 {
            metrics.record_home_latency(Duration::from_millis(value));
        }

        assert_eq!(metrics.home_latency_p95_ms(), Some(62));
    }

    #[test]
    fn background_concurrency_reserves_home_capacity_and_honors_limits() {
        assert_eq!(
            recommended_background_concurrency(8, 8, None, None, None),
            7
        );
        assert_eq!(
            recommended_background_concurrency(8, 8, Some(400), None, None),
            1
        );
        assert_eq!(
            recommended_background_concurrency(8, 16, None, Some(4.0), None),
            3
        );
        assert_eq!(
            recommended_background_concurrency(8, 8, None, None, Some(70.0)),
            4
        );
        assert_eq!(
            recommended_background_concurrency(8, 8, None, None, Some(85.0)),
            1
        );
    }

    #[test]
    fn probe_concurrency_uses_io_parallelism_before_backing_off() {
        assert_eq!(
            recommended_probe_concurrency(64, 4, None, None, None, None, 128),
            64
        );
        assert_eq!(
            recommended_probe_concurrency(128, 4, None, None, None, None, 128),
            64
        );
        assert_eq!(
            recommended_probe_concurrency(128, 8, None, None, None, None, 128),
            128
        );
    }

    #[test]
    fn probe_concurrency_backs_off_for_cpu_memory_and_frontend_pressure() {
        assert_eq!(
            recommended_probe_concurrency(128, 4, Some(1_000), None, None, None, 128),
            32
        );
        assert_eq!(
            recommended_probe_concurrency(128, 4, None, None, Some(90.0), None, 128),
            32
        );
        assert_eq!(
            recommended_probe_concurrency(128, 4, None, None, None, Some(90.0), 128),
            16
        );
    }

    #[test]
    fn probe_concurrency_recovers_in_steps_after_a_cooldown() {
        let mut state = super::ProbeConcurrencyState::default();
        let start = std::time::Instant::now();
        assert_eq!(
            super::stabilize_probe_concurrency(
                &mut state,
                128,
                128,
                start,
                Duration::from_secs(30),
            ),
            128
        );
        assert_eq!(
            super::stabilize_probe_concurrency(
                &mut state,
                16,
                64,
                start + Duration::from_secs(1),
                Duration::from_secs(30),
            ),
            16
        );
        assert_eq!(
            super::stabilize_probe_concurrency(
                &mut state,
                128,
                128,
                start + Duration::from_secs(2),
                Duration::from_secs(30),
            ),
            16
        );
        assert_eq!(
            super::stabilize_probe_concurrency(
                &mut state,
                128,
                128,
                start + Duration::from_secs(31),
                Duration::from_secs(30),
            ),
            32
        );
        assert_eq!(
            super::stabilize_probe_concurrency(
                &mut state,
                128,
                128,
                start + Duration::from_secs(62),
                Duration::from_secs(30),
            ),
            64
        );
        assert_eq!(
            super::stabilize_probe_concurrency(
                &mut state,
                128,
                128,
                start + Duration::from_secs(93),
                Duration::from_secs(30),
            ),
            128
        );
    }
}
