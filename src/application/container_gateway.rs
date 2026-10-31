//! Container runtime default-gateway detection.
//!
//! Under Docker bridge networking the host rewrites the source address of every published-port
//! connection: `docker-proxy` (or an iptables MASQUERADE rule) opens a fresh connection towards
//! the container, so the application can only observe the bridge gateway (`172.31.0.1`,
//! `172.18.0.1`, ...). Which gateway a deployment receives depends on the Docker address pool and
//! therefore cannot be written down as a fixed default.
//!
//! Lux resolves access IPs strictly: forwarded headers are only consulted when the connection peer
//! belongs to a trusted proxy. Treating the container's own default gateway as an implicit trusted
//! proxy restores `X-Forwarded-For` handling for every Docker deployment without asking the
//! administrator to look up a value that differs per host. Explicitly configured
//! `trustedProxyCidrs` stay additive and unchanged.
//!
//! The implicit trust is deliberately narrow:
//! - only enabled when the process is demonstrably running inside a container;
//! - only the single default-gateway address (`/32` or `/128`), never the whole bridge subnet;
//! - a missing, unspecified, multi-homed or unparsable route table yields no implicit trust.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;
use std::sync::OnceLock;

use super::user_access_rules::IpCidr;

const PROC_NET_ROUTE: &str = "/proc/net/route";
const PROC_NET_IPV6_ROUTE: &str = "/proc/net/ipv6_route";
const PROC_SELF_CGROUP: &str = "/proc/1/cgroup";
const DOCKERENV: &str = "/.dockerenv";
const CONTAINERENV: &str = "/run/.containerenv";

const ZERO_IPV6_HEX: &str = "00000000000000000000000000000000";

const RTF_UP: u16 = 0x0001;
const RTF_GATEWAY: u16 = 0x0002;

/// Warm the cached gateway detection at startup so the `/proc` lookups never run on Tokio
/// workers. Safe and cheap to call repeatedly.
pub fn warm_up() {
    let _ = implicit_trusted_proxy();
}

/// Implicit trusted proxy derived from the container runtime, resolved once per process.
pub(crate) fn implicit_trusted_proxy() -> Option<IpCidr> {
    static CACHE: OnceLock<Option<IpCidr>> = OnceLock::new();
    *CACHE.get_or_init(|| {
        detect_implicit_trusted_proxy(
            Path::new(DOCKERENV).exists(),
            Path::new(CONTAINERENV).exists(),
            std::fs::read_to_string(PROC_SELF_CGROUP).ok().as_deref(),
            std::fs::read_to_string(PROC_NET_ROUTE).ok().as_deref(),
            std::fs::read_to_string(PROC_NET_IPV6_ROUTE).ok().as_deref(),
        )
    })
}

/// Pure entry point used by tests and by the cached runtime check above.
pub(crate) fn detect_implicit_trusted_proxy(
    dockerenv_present: bool,
    containerenv_present: bool,
    cgroup: Option<&str>,
    ipv4_route_table: Option<&str>,
    ipv6_route_table: Option<&str>,
) -> Option<IpCidr> {
    if !looks_like_container(dockerenv_present, containerenv_present, cgroup) {
        return None;
    }
    let gateway = ipv4_route_table
        .and_then(parse_ipv4_default_gateway)
        .or_else(|| ipv6_route_table.and_then(parse_ipv6_default_gateway))?;
    let text = match gateway {
        IpAddr::V4(_) => format!("{gateway}/32"),
        IpAddr::V6(_) => format!("{gateway}/128"),
    };
    IpCidr::parse(&text).ok()
}

/// Container markers that survive in ordinary Docker, Podman, Kubernetes and LXC runtimes.
pub(crate) fn looks_like_container(
    dockerenv_present: bool,
    containerenv_present: bool,
    cgroup: Option<&str>,
) -> bool {
    if dockerenv_present || containerenv_present {
        return true;
    }
    cgroup.is_some_and(|contents| {
        let contents = contents.to_ascii_lowercase();
        [
            "docker",
            "kubepods",
            "containerd",
            "libpod",
            "lxc",
            "garden",
        ]
        .iter()
        .any(|marker| contents.contains(marker))
    })
}

/// Parse the IPv4 default gateway out of `/proc/net/route`.
///
/// The gateway column stores the address as a little-endian `u32` in hexadecimal, so
/// `0101A8C0` is `192.168.1.1`.
pub(crate) fn parse_ipv4_default_gateway(route_table: &str) -> Option<IpAddr> {
    for line in route_table.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let _interface = fields.next()?;
        let destination = fields.next()?;
        let gateway = fields.next()?;
        let flags = fields.next()?;
        let flags = u16::from_str_radix(flags, 16).ok()?;
        if !destination.eq_ignore_ascii_case("00000000")
            || gateway.eq_ignore_ascii_case("00000000")
            || flags & RTF_UP == 0
            || flags & RTF_GATEWAY == 0
        {
            continue;
        }
        if let Some(address) = parse_ipv4_route_address(gateway) {
            return Some(IpAddr::V4(address));
        }
    }
    None
}

fn parse_ipv4_route_address(value: &str) -> Option<Ipv4Addr> {
    if value.len() != 8 {
        return None;
    }
    let raw = u32::from_str_radix(value, 16).ok()?;
    let address = Ipv4Addr::from(raw.to_le_bytes());
    (!address.is_unspecified()).then_some(address)
}

/// Parse the IPv6 default gateway out of `/proc/net/ipv6_route`.
///
/// Columns: `dest prefix source sport nexthop metric refcnt use flags iface`; addresses are
/// big-endian hexadecimal and `flags` is hexadecimal.
pub(crate) fn parse_ipv6_default_gateway(route_table: &str) -> Option<IpAddr> {
    for line in route_table.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 9 {
            continue;
        }
        let destination = fields[0];
        let prefix = fields[1];
        let nexthop = fields[4];
        let flags = u32::from_str_radix(fields[8], 16).ok();
        if !destination.eq_ignore_ascii_case(ZERO_IPV6_HEX)
            || !prefix.eq_ignore_ascii_case("00")
            || flags.is_none_or(|flags| flags & u32::from(RTF_UP) == 0)
        {
            continue;
        }
        if let Some(address) = parse_ipv6_route_address(nexthop) {
            return Some(IpAddr::V6(address));
        }
    }
    None
}

fn parse_ipv6_route_address(value: &str) -> Option<Ipv6Addr> {
    if value.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (index, chunk) in value.as_bytes().chunks(2).enumerate() {
        let chunk = std::str::from_utf8(chunk).ok()?;
        bytes[index] = u8::from_str_radix(chunk, 16).ok()?;
    }
    let address = Ipv6Addr::from(bytes);
    (!address.is_unspecified()).then_some(address)
}

#[cfg(test)]
mod tests {
    use super::super::user_access_rules::resolve_client_ip_from_trusted_proxies;
    use super::*;

    const DOCKER_ROUTE_TABLE: &str = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t00000000\t01001FAC\t0003\t0\t0\t0\t00000000\t0\t0\t0
eth0\t0080A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0
";

    const LINK_SCOPED_DEFAULT: &str = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t00000000\t00000000\t0001\t0\t0\t0\t00000000\t0\t0\t0
";

    #[test]
    fn ipv4_default_gateway_is_decoded_from_little_endian_hex() {
        assert_eq!(
            parse_ipv4_default_gateway(DOCKER_ROUTE_TABLE),
            Some(IpAddr::V4(Ipv4Addr::new(172, 31, 0, 1)))
        );
    }

    #[test]
    fn ipv4_default_route_without_gateway_flag_yields_none() {
        assert_eq!(parse_ipv4_default_gateway(LINK_SCOPED_DEFAULT), None);
    }

    #[test]
    fn ipv4_route_table_without_default_route_yields_none() {
        let table = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t0080A8C0\t00000000\t0001\t0\t0\t0\t00FFFFFF\t0\t0\t0
";
        assert_eq!(parse_ipv4_default_gateway(table), None);
        assert_eq!(parse_ipv4_default_gateway(""), None);
        assert_eq!(parse_ipv4_default_gateway("Iface\tDestination\n"), None);
    }

    #[test]
    fn ipv6_default_gateway_is_decoded_from_big_endian_hex() {
        let table = "\
00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 00000400 00000000 00000000 00000001     eth0
00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000400 00000000 00000000 00000001     eth0
";
        assert_eq!(
            parse_ipv6_default_gateway(table),
            Some(IpAddr::V6("fe80::1".parse().expect("gateway")))
        );
    }

    #[test]
    fn ipv6_route_table_without_nexthop_yields_none() {
        let table = "\
00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 00000400 00000000 00000000 00000001     eth0
";
        assert_eq!(parse_ipv6_default_gateway(table), None);
    }

    #[test]
    fn container_detection_accepts_metadata_files_and_cgroup_markers() {
        assert!(looks_like_container(true, false, None));
        assert!(looks_like_container(false, true, None));
        assert!(looks_like_container(
            false,
            false,
            Some("0::/system.slice/docker-abc.scope")
        ));
        assert!(looks_like_container(
            false,
            false,
            Some("0::/kubepods/besteffort")
        ));
        assert!(!looks_like_container(false, false, Some("0::/init.scope")));
        assert!(!looks_like_container(false, false, None));
    }

    #[test]
    fn implicit_trust_is_only_applied_inside_containers() {
        let cidr = detect_implicit_trusted_proxy(
            false,
            false,
            Some("0::/init.scope"),
            Some(DOCKER_ROUTE_TABLE),
            None,
        );
        assert_eq!(cidr, None);
    }

    #[test]
    fn implicit_trust_matches_the_container_gateway_single_address() {
        let cidr = detect_implicit_trusted_proxy(
            true,
            false,
            Some("0::/docker/abc"),
            Some(DOCKER_ROUTE_TABLE),
            None,
        )
        .expect("docker deployment yields an implicit trusted proxy");
        assert_eq!(
            cidr,
            IpCidr::parse("172.31.0.1/32").expect("canonical cidr")
        );
    }

    #[test]
    fn implicit_gateway_unlocks_forwarded_for_for_the_bridge_peer() {
        let gateway = detect_implicit_trusted_proxy(
            true,
            false,
            Some("0::/docker/abc"),
            Some(
                "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                 eth0\t00000000\t01001FAC\t0003\t0\t0\t0\t00000000\t0\t0\t0\n",
            ),
            None,
        )
        .expect("implicit gateway");

        let peer = "172.31.0.1".parse().expect("peer");
        let resolved = resolve_client_ip_from_trusted_proxies(
            peer,
            &["39.183.141.86"],
            std::slice::from_ref(&gateway),
        );
        assert_eq!(resolved, Ok("39.183.141.86".parse().expect("client")));
    }

    #[test]
    fn implicit_gateway_does_not_make_unrelated_peers_trusted() {
        let gateway = detect_implicit_trusted_proxy(
            true,
            false,
            Some("0::/docker/abc"),
            Some(DOCKER_ROUTE_TABLE),
            None,
        )
        .expect("implicit gateway");

        let peer = "203.0.113.9".parse().expect("peer");
        let resolved = resolve_client_ip_from_trusted_proxies(
            peer,
            &["198.51.100.7"],
            std::slice::from_ref(&gateway),
        );
        assert_eq!(resolved, Ok(peer));
    }
}
