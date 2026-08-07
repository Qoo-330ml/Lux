use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{
    application::{plugin_protocol::IpLocationRpcResult, plugins::PluginService},
    network::is_public_address,
};

pub const SUCCESS_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const FAILURE_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_CACHE_ENTRIES: usize = 256;
const MAX_IN_FLIGHT: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpLocation {
    pub ip: String,
    pub country: Option<String>,
    pub province: Option<String>,
    pub city: Option<String>,
    pub district: Option<String>,
    pub street: Option<String>,
    pub isp: Option<String>,
    pub latitude: Option<String>,
    pub longitude: Option<String>,
}

impl IpLocation {
    pub fn formatted_location(&self) -> Option<String> {
        let parts = [&self.country, &self.province, &self.city]
            .into_iter()
            .filter_map(Option::as_deref)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum IpLocationError {
    InvalidIp,
    LookupFailed,
}

impl std::fmt::Display for IpLocationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidIp => "ip location input is invalid",
            Self::LookupFailed => "ip location lookup failed",
        })
    }
}

impl std::error::Error for IpLocationError {}

#[derive(Clone)]
pub struct IpLocationService {
    plugins: Option<PluginService>,
    cache: Arc<Mutex<CacheState>>,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<IpAddr, CacheEntry>,
    in_flight_ips: HashSet<IpAddr>,
    in_flight: usize,
}

struct CacheEntry {
    value: Option<IpLocation>,
    expires_at: Instant,
}

impl IpLocationService {
    pub fn new(plugins: PluginService) -> Self {
        Self {
            plugins: Some(plugins),
            cache: Arc::new(Mutex::new(CacheState::default())),
        }
    }

    pub fn cached_or_schedule(&self, raw_ip: &str) -> Option<IpLocation> {
        let ip = raw_ip.trim().parse::<IpAddr>().ok()?;
        if !is_public_address(ip) {
            return None;
        }

        let should_schedule = {
            let mut state = self.cache.lock().ok()?;
            let now = Instant::now();
            if let Some(entry) = state.entries.get(&ip) {
                if entry.expires_at > now {
                    return entry.value.clone();
                }
            }
            state.entries.remove(&ip);
            if state.in_flight_ips.contains(&ip) || state.in_flight >= MAX_IN_FLIGHT {
                false
            } else {
                state.in_flight_ips.insert(ip);
                state.in_flight += 1;
                true
            }
        };

        if should_schedule {
            let worker = self.clone();
            tokio::spawn(async move {
                let result = worker.lookup(ip).await;
                worker.store_result(ip, result);
            });
        }
        None
    }

    async fn lookup(&self, ip: IpAddr) -> Result<IpLocation, IpLocationError> {
        let plugins = self.plugins.as_ref().ok_or(IpLocationError::LookupFailed)?;
        let result = plugins
            .lookup_ip_location(ip)
            .await
            .map_err(|_| IpLocationError::LookupFailed)?;
        Ok(ip_location_from_rpc(result))
    }

    fn store_result(&self, ip: IpAddr, result: Result<IpLocation, IpLocationError>) {
        let value = result.ok();
        let expires_at = Instant::now()
            + if value.is_some() {
                SUCCESS_CACHE_TTL
            } else {
                FAILURE_CACHE_TTL
            };
        let Ok(mut state) = self.cache.lock() else {
            return;
        };
        state.in_flight_ips.remove(&ip);
        state.in_flight = state.in_flight.saturating_sub(1);
        if state.entries.len() >= MAX_CACHE_ENTRIES && !state.entries.contains_key(&ip) {
            if let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(oldest_ip, _)| *oldest_ip)
            {
                state.entries.remove(&oldest);
            }
        }
        state.entries.insert(ip, CacheEntry { value, expires_at });
    }

    #[cfg(test)]
    fn new_for_test() -> Self {
        Self {
            plugins: None,
            cache: Arc::new(Mutex::new(CacheState::default())),
        }
    }

    #[cfg(test)]
    fn store_result_for_test(&self, raw_ip: &str, result: Result<IpLocation, IpLocationError>) {
        if let Ok(ip) = raw_ip.parse::<IpAddr>() {
            self.store_result(ip, result);
        }
    }
}

fn ip_location_from_rpc(result: IpLocationRpcResult) -> IpLocation {
    IpLocation {
        ip: result.ip,
        country: result.country,
        province: result.province,
        city: result.city,
        district: result.district,
        street: result.street,
        isp: result.isp,
        latitude: result.latitude,
        longitude: result.longitude,
    }
}

pub fn is_public_lookup_address(raw_ip: &str) -> bool {
    raw_ip.trim().parse::<IpAddr>().is_ok_and(is_public_address)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{IpLocationService, SUCCESS_CACHE_TTL, is_public_lookup_address};

    #[test]
    fn only_public_addresses_are_eligible_for_lookup() {
        assert!(is_public_lookup_address("8.8.8.8"));
        assert!(is_public_lookup_address("2001:4860:4860::8888"));
        assert!(!is_public_lookup_address("127.0.0.1"));
        assert!(!is_public_lookup_address("192.168.1.20"));
        assert!(!is_public_lookup_address("fc00::1"));
        assert!(!is_public_lookup_address("not-an-ip"));
    }

    #[test]
    fn cached_success_is_returned_without_a_second_lookup_for_24_hours() {
        let service = IpLocationService::new_for_test();
        let result = super::IpLocation {
            ip: "8.8.8.8".to_owned(),
            country: Some("美国".to_owned()),
            province: Some("加利福尼亚州".to_owned()),
            city: Some("山景城".to_owned()),
            district: None,
            street: None,
            isp: Some("Google".to_owned()),
            latitude: None,
            longitude: None,
        };

        service.store_result_for_test("8.8.8.8", Ok(result.clone()));

        assert_eq!(service.cached_or_schedule("8.8.8.8"), Some(result));
        assert_eq!(SUCCESS_CACHE_TTL, Duration::from_secs(24 * 60 * 60));
    }

    #[test]
    fn does_not_schedule_the_same_ip_twice_while_the_first_lookup_is_running() {
        let service = IpLocationService::new_for_test();
        let ip = "8.8.8.8".parse().expect("valid IP");
        {
            let mut state = service.cache.lock().expect("cache should not be poisoned");
            state.in_flight = 1;
            state.in_flight_ips.insert(ip);
        }

        assert_eq!(service.cached_or_schedule("8.8.8.8"), None);
        assert_eq!(
            service
                .cache
                .lock()
                .expect("cache should not be poisoned")
                .in_flight,
            1
        );
    }
}
