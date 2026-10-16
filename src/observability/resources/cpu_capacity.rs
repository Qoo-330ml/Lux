use std::{
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

#[cfg(any(target_os = "linux", test))]
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

use tokio::sync::Mutex as AsyncMutex;

#[cfg(target_os = "linux")]
use tokio::{fs::File, io::AsyncReadExt};

const CPU_CAPACITY_CACHE_TTL: Duration = Duration::from_secs(30);
#[cfg(target_os = "linux")]
const MAX_PROC_FILE_BYTES: usize = 1024 * 1024;
#[cfg(target_os = "linux")]
const MAX_CGROUP_FILE_BYTES: usize = 64 * 1024;
#[cfg(any(target_os = "linux", test))]
const MAX_CGROUP_DEPTH: usize = 128;

static CPU_CAPACITY_CACHE: OnceLock<CpuCapacityCache> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CpuCapacitySource {
    Cgroup,
    AvailableParallelism,
    Fallback,
}

#[derive(Clone, Debug)]
pub(crate) struct CpuCapacity {
    pub(crate) effective_cores: f64,
    pub(crate) source: CpuCapacitySource,
    pub(crate) observed_at: Instant,
}

struct CpuCapacityCache {
    value: Mutex<Option<CpuCapacity>>,
    refresh: AsyncMutex<()>,
}

impl CpuCapacityCache {
    fn new() -> Self {
        Self {
            value: Mutex::new(None),
            refresh: AsyncMutex::new(()),
        }
    }

    fn snapshot(&self) -> Option<CpuCapacity> {
        self.value.lock().ok()?.clone()
    }

    fn store(&self, value: CpuCapacity) {
        if let Ok(mut cached) = self.value.lock() {
            *cached = Some(value);
        }
    }
}

pub(crate) async fn current_cpu_capacity() -> CpuCapacity {
    let cache = CPU_CAPACITY_CACHE.get_or_init(CpuCapacityCache::new);
    let now = Instant::now();
    if let Some(value) = cache.snapshot()
        && now.saturating_duration_since(value.observed_at) < CPU_CAPACITY_CACHE_TTL
    {
        return value;
    }

    let _refresh_guard = match cache.refresh.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            if let Some(stale) = cache.snapshot() {
                return stale;
            }
            cache.refresh.lock().await
        }
    };
    let now = Instant::now();
    if let Some(value) = cache.snapshot()
        && now.saturating_duration_since(value.observed_at) < CPU_CAPACITY_CACHE_TTL
    {
        return value;
    }

    let value = discover_cpu_capacity().await;
    cache.store(value.clone());
    value
}

pub(crate) async fn automatic_probe_concurrency() -> usize {
    let capacity = current_cpu_capacity().await;
    automatic_probe_budget(capacity.effective_cores)
}

async fn discover_cpu_capacity() -> CpuCapacity {
    let available = tokio::task::spawn_blocking(|| {
        thread::available_parallelism()
            .ok()
            .map(|parallelism| parallelism.get())
    })
    .await
    .ok()
    .flatten();

    #[cfg(target_os = "linux")]
    let (quotas, cpusets) = linux_cgroup_constraints().await;
    #[cfg(not(target_os = "linux"))]
    let (quotas, cpusets) = (Vec::new(), Vec::new());

    let effective_cores = effective_capacity(available, &quotas, &cpusets);
    let source = if !quotas.is_empty() || !cpusets.is_empty() {
        CpuCapacitySource::Cgroup
    } else if available.is_some() {
        CpuCapacitySource::AvailableParallelism
    } else {
        CpuCapacitySource::Fallback
    };
    CpuCapacity {
        effective_cores,
        source,
        observed_at: Instant::now(),
    }
}

fn automatic_probe_budget(effective_cores: f64) -> usize {
    if !effective_cores.is_finite() || effective_cores <= 0.0 {
        return 1;
    }
    (effective_cores * 4.0)
        .ceil()
        .clamp(1.0, crate::config::MAX_PROBE_CONCURRENCY as f64) as usize
}

fn effective_capacity(available: Option<usize>, quotas: &[f64], cpusets: &[usize]) -> f64 {
    let mut capacity = available.map_or(f64::INFINITY, |cores| cores.max(1) as f64);
    let mut has_capacity = available.is_some();
    for quota in quotas
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        capacity = capacity.min(quota);
        has_capacity = true;
    }
    for cpuset in cpusets.iter().copied().filter(|value| *value > 0) {
        capacity = capacity.min(cpuset as f64);
        has_capacity = true;
    }
    if has_capacity && capacity.is_finite() && capacity > 0.0 {
        capacity
    } else {
        1.0
    }
}

fn parse_cpu_quota_v2(input: &str) -> Option<f64> {
    let mut fields = input.split_whitespace();
    let quota = fields.next()?;
    let period = fields.next()?.parse::<u64>().ok()?;
    if quota == "max" || period == 0 {
        return None;
    }
    let quota = quota.parse::<u64>().ok()?;
    (quota > 0).then_some(quota as f64 / period as f64)
}

fn parse_cpu_quota_v1(quota: &str, period: &str) -> Option<f64> {
    let quota = quota.trim().parse::<i64>().ok()?;
    let period = period.trim().parse::<u64>().ok()?;
    if quota <= 0 || period == 0 {
        return None;
    }
    Some(quota as f64 / period as f64)
}

fn parse_cpuset(input: &str) -> Option<usize> {
    let mut ranges = Vec::new();
    for part in input.trim().split(',') {
        if part.is_empty() {
            return None;
        }
        let mut bounds = part.split('-');
        let start = bounds.next()?.parse::<u32>().ok()?;
        let end = match bounds.next() {
            Some(end) => end.parse::<u32>().ok()?,
            None => start,
        };
        if bounds.next().is_some() || start > end {
            return None;
        }
        ranges.push((start, end));
    }
    if ranges.is_empty() {
        return None;
    }
    ranges.sort_unstable();
    let mut total = 0_u64;
    let mut current = ranges[0];
    for range in ranges.into_iter().skip(1) {
        if u64::from(range.0) <= u64::from(current.1).saturating_add(1) {
            current.1 = current.1.max(range.1);
        } else {
            total = total.saturating_add(u64::from(current.1) - u64::from(current.0) + 1);
            current = range;
        }
    }
    total = total.saturating_add(u64::from(current.1) - u64::from(current.0) + 1);
    usize::try_from(total).ok().filter(|count| *count > 0)
}

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CgroupVersion {
    V1,
    V2,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
struct CgroupMembership {
    version: CgroupVersion,
    controllers: HashSet<String>,
    path: PathBuf,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug)]
struct CgroupMount {
    version: CgroupVersion,
    controllers: HashSet<String>,
    root: PathBuf,
    mount_point: PathBuf,
}

#[cfg(target_os = "linux")]
async fn linux_cgroup_constraints() -> (Vec<f64>, Vec<usize>) {
    let Some(membership_text) = read_bounded("/proc/self/cgroup", MAX_PROC_FILE_BYTES).await else {
        return (Vec::new(), Vec::new());
    };
    let Some(mount_text) = read_bounded("/proc/self/mountinfo", MAX_PROC_FILE_BYTES).await else {
        return (Vec::new(), Vec::new());
    };
    let memberships = parse_cgroup_memberships(&membership_text);
    let mounts = parse_cgroup_mounts(&mount_text);
    let mut quotas = Vec::new();
    let mut cpusets = Vec::new();

    for membership in &memberships {
        for mount in mounts
            .iter()
            .filter(|mount| mount_matches(membership, mount))
        {
            let Some(base) = cgroup_directory(membership, mount) else {
                continue;
            };
            let Some(ancestors) = ancestor_directories(&base, &mount.mount_point) else {
                continue;
            };
            if membership.version == CgroupVersion::V2 {
                for directory in &ancestors {
                    if let Some(value) = read_cgroup_file(directory, "cpu.max").await
                        && let Some(limit) = parse_cpu_quota_v2(&value)
                    {
                        quotas.push(limit);
                    }
                }
            } else if membership.controllers.contains("cpu") {
                for directory in &ancestors {
                    let quota = read_cgroup_file(directory, "cpu.cfs_quota_us").await;
                    let period = read_cgroup_file(directory, "cpu.cfs_period_us").await;
                    if let (Some(quota), Some(period)) = (quota, period)
                        && let Some(limit) = parse_cpu_quota_v1(&quota, &period)
                    {
                        quotas.push(limit);
                    }
                }
            }

            let cpuset_files: &[&str] = if membership.version == CgroupVersion::V2 {
                &["cpuset.cpus.effective", "cpuset.cpus"]
            } else {
                &["cpuset.effective_cpus", "cpuset.cpus"]
            };
            for directory in &ancestors {
                let mut found = false;
                for name in cpuset_files {
                    if let Some(value) = read_cgroup_file(directory, name).await
                        && let Some(count) = parse_cpuset(&value)
                    {
                        cpusets.push(count);
                        found = true;
                        break;
                    }
                }
                if found {
                    break;
                }
            }
        }
    }
    (quotas, cpusets)
}

#[cfg(any(target_os = "linux", test))]
fn parse_cgroup_memberships(input: &str) -> Vec<CgroupMembership> {
    input
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ':');
            let _hierarchy = fields.next()?;
            let controllers = fields.next()?;
            let path = PathBuf::from(fields.next()?);
            if !path.is_absolute() || has_parent_component(&path) {
                return None;
            }
            let version = if controllers.is_empty() {
                CgroupVersion::V2
            } else {
                CgroupVersion::V1
            };
            Some(CgroupMembership {
                version,
                controllers: controllers
                    .split(',')
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect(),
                path,
            })
        })
        .collect()
}

#[cfg(any(target_os = "linux", test))]
fn parse_cgroup_mounts(input: &str) -> Vec<CgroupMount> {
    input
        .lines()
        .filter_map(|line| {
            let (before, after) = line.split_once(" - ")?;
            let before = before.split_whitespace().collect::<Vec<_>>();
            let after = after.split_whitespace().collect::<Vec<_>>();
            let root = PathBuf::from(unescape_mount_path(before.get(3)?));
            let mount_point = PathBuf::from(unescape_mount_path(before.get(4)?));
            let filesystem = *after.first()?;
            let version = match filesystem {
                "cgroup2" => CgroupVersion::V2,
                "cgroup" => CgroupVersion::V1,
                _ => return None,
            };
            if !root.is_absolute() || !mount_point.is_absolute() {
                return None;
            }
            let controllers = after
                .get(2)
                .into_iter()
                .flat_map(|values| values.split(','))
                .filter(|value| matches!(*value, "cpu" | "cpuacct" | "cpuset"))
                .map(str::to_owned)
                .collect();
            Some(CgroupMount {
                version,
                controllers,
                root,
                mount_point,
            })
        })
        .collect()
}

#[cfg(any(target_os = "linux", test))]
fn mount_matches(membership: &CgroupMembership, mount: &CgroupMount) -> bool {
    membership.version == mount.version
        && match membership.version {
            CgroupVersion::V2 => true,
            CgroupVersion::V1 => membership
                .controllers
                .iter()
                .any(|controller| mount.controllers.contains(controller)),
        }
}

#[cfg(any(target_os = "linux", test))]
fn cgroup_directory(membership: &CgroupMembership, mount: &CgroupMount) -> Option<PathBuf> {
    let relative = membership.path.strip_prefix(&mount.root).ok()?;
    if has_parent_component(relative) {
        return None;
    }
    Some(mount.mount_point.join(relative))
}

#[cfg(any(target_os = "linux", test))]
fn ancestor_directories(base: &Path, mount_point: &Path) -> Option<Vec<PathBuf>> {
    if !base.starts_with(mount_point) || has_parent_component(base) {
        return None;
    }
    let mut current = base.to_path_buf();
    let mut ancestors = Vec::new();
    for _ in 0..MAX_CGROUP_DEPTH {
        ancestors.push(current.clone());
        if current == mount_point {
            return Some(ancestors);
        }
        if !current.pop() || !current.starts_with(mount_point) {
            return None;
        }
    }
    None
}

#[cfg(any(target_os = "linux", test))]
fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
}

#[cfg(any(target_os = "linux", test))]
fn unescape_mount_path(value: &str) -> String {
    value
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

#[cfg(target_os = "linux")]
async fn read_cgroup_file(directory: &Path, name: &str) -> Option<String> {
    read_bounded(directory.join(name), MAX_CGROUP_FILE_BYTES).await
}

#[cfg(target_os = "linux")]
async fn read_bounded(path: impl AsRef<Path>, max_bytes: usize) -> Option<String> {
    let file = File::open(path).await.ok()?;
    let mut limited = file.take(u64::try_from(max_bytes.saturating_add(1)).ok()?);
    let mut bytes = Vec::new();
    limited.read_to_end(&mut bytes).await.ok()?;
    if bytes.len() > max_bytes {
        return None;
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::{
        automatic_probe_budget, current_cpu_capacity, effective_capacity, parse_cpu_quota_v1,
        parse_cpu_quota_v2, parse_cpuset,
    };

    #[test]
    fn cpu_quota_parsers_keep_fractional_capacity_and_ignore_unlimited_values() {
        assert_eq!(parse_cpu_quota_v2("50000 100000"), Some(0.5));
        assert_eq!(parse_cpu_quota_v2("max 100000"), None);
        assert_eq!(parse_cpu_quota_v1("150000", "100000"), Some(1.5));
        assert_eq!(parse_cpu_quota_v1("-1", "100000"), None);
    }

    #[test]
    fn cpuset_ranges_count_unique_cpu_ids() {
        assert_eq!(parse_cpuset("0-3,2-5,8"), Some(7));
        assert_eq!(parse_cpuset(""), None);
        assert_eq!(parse_cpuset("0-4,invalid"), None);
    }

    #[test]
    fn effective_capacity_intersects_affinity_quota_and_cpuset_limits() {
        assert_eq!(effective_capacity(Some(8), &[4.0, 3.5], &[6, 4]), 3.5);
        assert_eq!(effective_capacity(None, &[], &[2]), 2.0);
        assert_eq!(effective_capacity(None, &[0.5], &[]), 0.5);
        assert_eq!(effective_capacity(None, &[], &[]), 1.0);
    }

    #[test]
    fn automatic_budget_rounds_fractional_cores_up_and_stays_bounded() {
        assert_eq!(automatic_probe_budget(0.25), 1);
        assert_eq!(automatic_probe_budget(1.25), 5);
        assert_eq!(automatic_probe_budget(4.0), 16);
        assert_eq!(automatic_probe_budget(200.0), 512);
        assert_eq!(automatic_probe_budget(f64::NAN), 1);
    }

    #[tokio::test]
    async fn host_capacity_produces_a_bounded_automatic_budget_without_cpu_metrics() {
        let capacity = current_cpu_capacity().await;
        let _source = capacity.source;
        assert!(capacity.effective_cores.is_finite());
        assert!(capacity.effective_cores > 0.0);
        assert!((1..=512).contains(&automatic_probe_budget(capacity.effective_cores)));
    }

    #[test]
    fn cgroup_membership_is_mapped_relative_to_each_mount_root() {
        use super::{
            CgroupVersion, ancestor_directories, cgroup_directory, mount_matches,
            parse_cgroup_memberships, parse_cgroup_mounts,
        };
        use std::path::PathBuf;

        let memberships = parse_cgroup_memberships(
            "0::/tenant/service\n4:cpu,cpuacct:/tenant/service\n5:cpuset:/tenant/service\n",
        );
        let mounts = parse_cgroup_mounts(
            "29 20 0:27 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n30 20 0:28 /tenant /sys/fs/cgroup/cpu rw - cgroup cgroup rw,cpu,cpuacct\n31 20 0:29 /tenant /sys/fs/cgroup/cpuset rw - cgroup cgroup rw,cpuset\n",
        );

        let unified = memberships
            .iter()
            .find(|membership| membership.version == CgroupVersion::V2)
            .and_then(|membership| {
                mounts
                    .iter()
                    .find(|mount| mount.version == CgroupVersion::V2)
                    .filter(|mount| mount_matches(membership, mount))
                    .and_then(|mount| cgroup_directory(membership, mount))
            });
        assert_eq!(
            unified,
            Some(PathBuf::from("/sys/fs/cgroup/tenant/service"))
        );

        let cpuset = memberships
            .iter()
            .find(|membership| membership.controllers.contains("cpuset"))
            .and_then(|membership| {
                mounts
                    .iter()
                    .find(|mount| mount.controllers.contains("cpuset"))
                    .filter(|mount| mount_matches(membership, mount))
                    .and_then(|mount| cgroup_directory(membership, mount))
            });
        assert_eq!(cpuset, Some(PathBuf::from("/sys/fs/cgroup/cpuset/service")));

        let cpu = memberships
            .iter()
            .find(|membership| membership.controllers.contains("cpu"))
            .and_then(|membership| {
                mounts
                    .iter()
                    .find(|mount| mount.controllers.contains("cpu"))
                    .filter(|mount| mount_matches(membership, mount))
                    .and_then(|mount| cgroup_directory(membership, mount))
            });
        assert_eq!(cpu, Some(PathBuf::from("/sys/fs/cgroup/cpu/service")));

        let cpu_mount = mounts
            .iter()
            .find(|mount| mount.controllers.contains("cpu"))
            .expect("cpu fixture mount");
        assert_eq!(
            ancestor_directories(
                &PathBuf::from("/sys/fs/cgroup/cpu/service"),
                &cpu_mount.mount_point,
            ),
            Some(vec![
                PathBuf::from("/sys/fs/cgroup/cpu/service"),
                PathBuf::from("/sys/fs/cgroup/cpu"),
            ])
        );
        assert!(parse_cgroup_memberships("0::/../escape").is_empty());
    }
}
