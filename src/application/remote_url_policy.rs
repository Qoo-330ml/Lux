//! Shared building blocks for every outbound URL policy.
//!
//! Lux makes outbound connections from several independent features (STRM playback, webhooks,
//! image scraping and the Emby migration) and each of them has its own deliberate security
//! boundary: STRM must keep LAN playback reachable, webhooks default to public destinations only,
//! images follow scraper rules, and the Emby migration is a host-side literal gate in front of a
//! plugin process. What they share is the mechanical part - which IPv4/IPv6 range an address
//! belongs to, how an embedded IPv4 form folds back, how a trailing root dot must not turn
//! `localhost` into a different name, how a resolver answer is checked as a whole, and how that
//! answer is pinned onto the connection that will actually be opened.
//!
//! That mechanical part lives here as small named predicates and helpers; the policies stay with
//! their callers, so no single boolean-parameterized gate has to serve four different rules.

use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
};

use reqwest::ClientBuilder;
use url::{Host, Url};

/// Local-only names that a self-hosted install never publishes an outbound target on.
///
/// Callers compare against a [`OutboundHost::Domain`] `normalized` name, which has already lost its
/// trailing root dot and ASCII case, so `router.local.`, `ROUTER.LOCAL` and `router.local` are one
/// name here.
pub(crate) const LOCAL_HOST_SUFFIXES: [&str; 4] = ["localhost", "local", "internal", "home.arpa"];

/// The typed host of an outbound URL, with the normalization every policy needs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OutboundHost {
    Ipv4(Ipv4Addr),
    Ipv6(Ipv6Addr),
    /// A name, never an address.
    Domain {
        /// The authority exactly as the request carries it, including a trailing root dot. This is
        /// also the key of the resolver override that pins the checked address set.
        request_host: String,
        /// The same name with ASCII case folded and the trailing root dot removed, for policy
        /// comparisons and messages.
        normalized: String,
    },
}

impl OutboundHost {
    /// Classifies the host of `url`.
    ///
    /// Returns `None` when the URL has no host and when a name normalizes to nothing (`.`), which
    /// is not a destination any policy may approve. Callers map that to their own error.
    pub(crate) fn from_url(url: &Url) -> Option<Self> {
        match url.host()? {
            Host::Ipv4(address) => Some(Self::Ipv4(address)),
            Host::Ipv6(address) => Some(Self::Ipv6(address)),
            Host::Domain(domain) => {
                let request_host = domain.to_owned();
                let normalized = normalize_domain(&request_host)?;
                Some(Self::Domain {
                    request_host,
                    normalized,
                })
            }
        }
    }

    /// The address of a host literal. A name has no address before the resolver answer.
    pub(crate) fn literal_address(&self) -> Option<IpAddr> {
        match self {
            Self::Ipv4(address) => Some(IpAddr::V4(*address)),
            Self::Ipv6(address) => Some(IpAddr::V6(*address)),
            Self::Domain { .. } => None,
        }
    }

    /// The authority as the request carries it, or `None` for a literal.
    ///
    /// A literal cannot be redirected by DNS, so it never needs a resolver override.
    pub(crate) fn request_host(&self) -> Option<&str> {
        match self {
            Self::Ipv4(_) | Self::Ipv6(_) => None,
            Self::Domain { request_host, .. } => Some(request_host),
        }
    }
}

/// Folds ASCII case and drops the trailing root dot; an empty result is not a usable name.
///
/// `localhost.` is the same name to a DNS resolver as `localhost`, so every policy compares the
/// normalized form instead of the raw authority.
fn normalize_domain(host: &str) -> Option<String> {
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    (!normalized.is_empty()).then_some(normalized)
}

/// Whether `host` equals one of `suffixes` or is a subdomain of it.
///
/// The comparison is label-aware: a suffix only matches at a dot boundary, so `notlocalhost` is not
/// `localhost`. Callers pass an already normalized name.
pub(crate) fn domain_matches_any(host: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suffix| {
        host == *suffix
            || host
                .strip_suffix(suffix)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

/// Returns the IPv4 target an IP address actually reaches, if it wraps one.
///
/// `::ffff:0:0/96` (IPv4-mapped) and the deprecated `::/96` (IPv4-compatible) forms both end up as a
/// plain IPv4 connection, so a policy that classified only the IPv6 shape could be bypassed with
/// `::ffff:127.0.0.1`. `::` and `::1` are real IPv6 addresses and keep their IPv6 meaning.
pub(crate) fn unwrap_embedded_ipv4(address: IpAddr) -> Option<Ipv4Addr> {
    let IpAddr::V6(address) = address else {
        return None;
    };
    address
        .to_ipv4_mapped()
        .or_else(|| ipv4_compatible_target(address))
}

/// The address a policy must classify: an embedded IPv4 form folds back to its IPv4 target.
pub(crate) fn canonical_ip(address: IpAddr) -> IpAddr {
    unwrap_embedded_ipv4(address).map_or(address, IpAddr::V4)
}

/// The socket form of [`canonical_ip`], keeping the port.
pub(crate) fn unwrap_embedded_socket_address(address: SocketAddr) -> SocketAddr {
    match unwrap_embedded_ipv4(address.ip()) {
        Some(ipv4) => SocketAddr::new(IpAddr::V4(ipv4), address.port()),
        None => address,
    }
}

/// Returns the IPv4 target embedded in a deprecated IPv4-compatible (`::/96`) address.
///
/// IPv4-compatible addresses are still handed out by DNS, so they must not slip past the IPv4
/// policy. `::` (unspecified) and `::1` (loopback) are real IPv6 addresses and keep their IPv6
/// meaning instead of being unwrapped into `0.0.0.0` and `0.0.0.1`.
pub(crate) fn ipv4_compatible_target(address: Ipv6Addr) -> Option<Ipv4Addr> {
    if address.is_unspecified() || address.is_loopback() {
        return None;
    }
    let segments = address.segments();
    if segments[0..6].iter().any(|segment| *segment != 0) {
        return None;
    }
    let [high, low] = [segments[6], segments[7]];
    Some(Ipv4Addr::new(
        (high >> 8) as u8,
        (high & 0xff) as u8,
        (low >> 8) as u8,
        (low & 0xff) as u8,
    ))
}

/// `0.0.0.0/8` ("this network"): never a unicast destination.
pub(crate) fn ipv4_is_this_network(address: Ipv4Addr) -> bool {
    address.octets()[0] == 0
}

/// `100.64.0.0/10` (carrier-grade NAT). Self-hosted installs use it for Tailscale and ZeroTier,
/// which is why STRM deliberately does not treat it as reserved.
pub(crate) fn ipv4_is_carrier_grade_nat(address: Ipv4Addr) -> bool {
    let [first, second, ..] = address.octets();
    first == 100 && (second & 0b1100_0000) == 0b0100_0000
}

/// `192.0.0.0/24`: IETF protocol assignments, never an endpoint.
pub(crate) fn ipv4_is_ietf_protocol_assignment(address: Ipv4Addr) -> bool {
    matches!(address.octets(), [192, 0, 0, _])
}

/// `198.18.0.0/15`: benchmarking only.
pub(crate) fn ipv4_is_benchmark(address: Ipv4Addr) -> bool {
    matches!(address.octets(), [198, 18 | 19, _, _])
}

/// `192.88.99.0/24`: the deprecated 6to4 relay anycast prefix.
pub(crate) fn ipv4_is_6to4_relay_anycast(address: Ipv4Addr) -> bool {
    matches!(address.octets(), [192, 88, 99, _])
}

/// `192.0.2.0/24`, `198.51.100.0/24` and `203.0.113.0/24`: the RFC 5737 documentation test nets.
pub(crate) fn ipv4_is_documentation(address: Ipv4Addr) -> bool {
    matches!(
        address.octets(),
        [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _]
    )
}

/// `240.0.0.0/4`: reserved for future use, including the limited broadcast address.
pub(crate) fn ipv4_is_reserved(address: Ipv4Addr) -> bool {
    address.octets()[0] >= 240
}

/// `2002::/16` (6to4), `2001::/32` (Teredo) and `64:ff9b::/96` (NAT64).
///
/// Each of them reaches or relays to an arbitrary IPv4 destination, so none is a plain IPv6 peer
/// and a policy cannot classify it as one.
pub(crate) fn ipv6_is_tunnel(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    segments[0] == 0x2002
        || (segments[0] == 0x2001 && segments[1] == 0)
        || (segments[0] == 0x0064 && segments[1] == 0xff9b)
}

/// `2001:db8::/32`: the IPv6 documentation range, never routable.
pub(crate) fn ipv6_is_documentation(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    segments[0] == 0x2001 && segments[1] == 0x0db8
}

/// `100::/64`: the discard-only prefix.
pub(crate) fn ipv6_is_discard_only(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0
}

/// Whether an address is a private, shared or otherwise reserved destination that no public
/// outbound target may reach.
///
/// This is the image and Emby classification: RFC 1918, loopback, link-local, carrier-grade NAT,
/// ULA, the special-purpose IPv4 blocks and every tunnelled or documentation range are all rejected.
/// STRM and webhooks use narrower compositions of the same predicates because they allow some of
/// these ranges by design.
pub(crate) fn is_private_or_reserved(address: IpAddr) -> bool {
    match canonical_ip(address) {
        IpAddr::V4(address) => ipv4_is_private_or_reserved(address),
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_multicast()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                || ipv6_is_tunnel(address)
                || ipv6_is_documentation(address)
                || ipv6_is_discard_only(address)
        }
    }
}

fn ipv4_is_private_or_reserved(address: Ipv4Addr) -> bool {
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_multicast()
        || ipv4_is_this_network(address)
        || ipv4_is_carrier_grade_nat(address)
        || ipv4_is_ietf_protocol_assignment(address)
        || ipv4_is_benchmark(address)
        || ipv4_is_6to4_relay_anycast(address)
        || ipv4_is_documentation(address)
        || ipv4_is_reserved(address)
}

/// A future returned by one outbound host lookup.
pub(crate) type HostLookupFuture<'a> =
    Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>>;

/// Resolves one host name to every candidate address a connection may use.
///
/// The trait is the injection point for scripted DNS answers in tests; production code uses
/// [`SystemHostResolver`].
pub(crate) trait HostResolver: Send + Sync {
    fn resolve(&self, host: String, port: u16) -> HostLookupFuture<'static>;
}

impl<F> HostResolver for F
where
    F: Fn(String, u16) -> HostLookupFuture<'static> + Send + Sync,
{
    fn resolve(&self, host: String, port: u16) -> HostLookupFuture<'static> {
        self(host, port)
    }
}

/// The production resolver: the operating system's name service through Tokio.
pub(crate) struct SystemHostResolver;

impl HostResolver for SystemHostResolver {
    fn resolve(&self, host: String, port: u16) -> HostLookupFuture<'static> {
        Box::pin(async move {
            tokio::net::lookup_host((host.as_str(), port))
                .await
                .map(|addresses| addresses.collect())
        })
    }
}

/// Why a resolver answer was rejected by the caller's address policy.
#[derive(Debug)]
pub(crate) enum AddressCheckError {
    /// The answer held no address, so there is nothing to connect to.
    Empty,
    /// At least one candidate failed the policy, which rejects the whole answer.
    Rejected,
}

/// Why resolving a host name to a pin-ready address set failed.
#[derive(Debug)]
pub(crate) enum HostResolutionError {
    /// The lookup itself failed.
    Lookup(std::io::Error),
    /// The lookup answered, but the answer was not usable.
    Check(AddressCheckError),
}

/// Resolves `host` and returns only the checked addresses a client may be pinned to.
///
/// The caller owns the policy through `permits`; this helper owns the mechanics: there must be at
/// least one address, every candidate must pass, and the result is deduplicated.
pub(crate) async fn resolve_checked_addresses(
    resolver: &dyn HostResolver,
    host: &str,
    port: u16,
    permits: impl Fn(IpAddr) -> bool,
) -> Result<Vec<SocketAddr>, HostResolutionError> {
    let candidates = resolver
        .resolve(host.to_owned(), port)
        .await
        .map_err(HostResolutionError::Lookup)?;
    checked_addresses(candidates, permits).map_err(HostResolutionError::Check)
}

/// Checks a complete resolver answer and returns the pin-ready address set.
///
/// A single forbidden candidate rejects the whole answer: otherwise an attacker could race a mixed
/// response or rebind between the check and the connection. The remaining checked addresses are all
/// kept (order preserved, duplicates collapsed, embedded IPv4 forms unfolded) so the client can
/// still fall back between them, which keeps dual-stack and dead-peer resilience without ever
/// touching an unchecked address.
pub(crate) fn checked_addresses(
    candidates: Vec<SocketAddr>,
    permits: impl Fn(IpAddr) -> bool,
) -> Result<Vec<SocketAddr>, AddressCheckError> {
    if candidates.is_empty() {
        return Err(AddressCheckError::Empty);
    }
    if candidates.iter().any(|candidate| !permits(candidate.ip())) {
        return Err(AddressCheckError::Rejected);
    }
    let mut addresses = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let address = unwrap_embedded_socket_address(candidate);
        if !addresses.contains(&address) {
            addresses.push(address);
        }
    }
    Ok(addresses)
}

/// Pins a checked resolver answer onto the client that opens the connection.
///
/// Reqwest keys its resolver override by the literal authority, so `media.example` and
/// `media.example.` are two keys for one host. Pinning only the spelling the URL happens to carry
/// would let the other spelling fall back to a fresh, unchecked system answer, so both are pinned
/// whenever they differ.
pub(crate) fn pin_resolved_addresses(
    builder: ClientBuilder,
    host: &str,
    addresses: &[SocketAddr],
) -> ClientBuilder {
    let (authority, without_root_dot) = resolver_override_hosts(host);
    let builder = builder.resolve_to_addrs(authority, addresses);
    match without_root_dot {
        Some(alias) => builder.resolve_to_addrs(alias, addresses),
        None => builder,
    }
}

/// The authority spellings a resolver override must be registered for.
fn resolver_override_hosts(host: &str) -> (&str, Option<&str>) {
    let without_root_dot = host.trim_end_matches('.');
    let alias = (without_root_dot.len() < host.len() && !without_root_dot.is_empty())
        .then_some(without_root_dot);
    (host, alias)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    use reqwest::Url;

    use super::{
        AddressCheckError, LOCAL_HOST_SUFFIXES, OutboundHost, canonical_ip, checked_addresses,
        domain_matches_any, is_private_or_reserved, resolver_override_hosts,
        unwrap_embedded_socket_address,
    };

    #[test]
    fn mapped_and_tunnelled_addresses_are_treated_as_private() {
        for value in [
            "127.0.0.1",
            "10.0.0.5",
            "169.254.169.254",
            "240.0.0.1",
            "255.255.255.255",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.5",
            "::127.0.0.1",
            "::192.168.1.1",
            "::169.254.169.254",
            "64:ff9b::7f00:1",
            "2002:7f00:1::",
            "2001:db8::1",
            "fc00::1",
            "fe80::1",
        ] {
            let address: IpAddr = value.parse().expect("address");
            assert!(is_private_or_reserved(address), "{value} must be blocked");
        }
        for value in [
            "1.1.1.1",
            "8.8.8.8",
            "2606:4700:4700::1111",
            "::93.184.216.34",
        ] {
            let address: IpAddr = value.parse().expect("address");
            assert!(
                !is_private_or_reserved(address),
                "{value} must stay allowed"
            );
        }
    }

    #[test]
    fn outbound_hosts_fold_case_and_strip_the_trailing_root_dot() {
        let url = Url::parse("https://Media.Example.:8443/poster.jpg").expect("url");
        assert_eq!(
            OutboundHost::from_url(&url),
            Some(OutboundHost::Domain {
                request_host: "media.example.".to_owned(),
                normalized: "media.example".to_owned(),
            })
        );

        let literal = Url::parse("http://[::ffff:10.0.0.5]:8080/video.mkv").expect("url");
        assert_eq!(
            OutboundHost::from_url(&literal),
            Some(OutboundHost::Ipv6(Ipv6Addr::new(
                0, 0, 0, 0, 0, 0xffff, 0x0a00, 0x0005
            )))
        );
        // A literal is never pinned: DNS cannot redirect it.
        let host = OutboundHost::from_url(&literal).expect("the literal host should classify");
        assert_eq!(host.request_host(), None);

        // The root label alone is not a destination any policy may approve.
        let root = Url::parse("http://./hook").expect("url");
        assert_eq!(OutboundHost::from_url(&root), None);
    }

    #[test]
    fn domain_suffixes_only_match_at_a_label_boundary() {
        for host in ["localhost", "emby.localhost", "nas.local", "box.home.arpa"] {
            assert!(
                domain_matches_any(host, &LOCAL_HOST_SUFFIXES),
                "{host} must count as a local name"
            );
        }
        for host in [
            "notlocalhost",
            "local.example",
            "internal.example.com",
            "media.example.test",
        ] {
            assert!(
                !domain_matches_any(host, &LOCAL_HOST_SUFFIXES),
                "{host} must not count as a local name"
            );
        }
    }

    #[test]
    fn canonical_addresses_unwrap_embedded_ipv4_forms() {
        for (value, expected) in [
            ("::ffff:127.0.0.1", Ipv4Addr::new(127, 0, 0, 1)),
            ("::192.168.1.1", Ipv4Addr::new(192, 168, 1, 1)),
        ] {
            let address: IpAddr = value.parse().expect("address");
            assert_eq!(canonical_ip(address), IpAddr::V4(expected));
        }
        // `::` and `::1` keep their IPv6 meaning.
        for value in ["::", "::1"] {
            let address: IpAddr = value.parse().expect("address");
            assert_eq!(canonical_ip(address), address);
        }
        assert_eq!(
            unwrap_embedded_socket_address(SocketAddr::new(
                IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0a00, 0x0005)),
                8080
            )),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), 8080)
        );
    }

    #[test]
    fn checked_answers_reject_empty_and_mixed_resolutions() {
        let public = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), 443);
        let lan = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)), 8080);
        let mapped_lan = SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0a00, 0x0005)),
            8080,
        );
        let permitted = |address: IpAddr| !is_private_or_reserved(address);

        assert!(matches!(
            checked_addresses(Vec::new(), permitted),
            Err(AddressCheckError::Empty)
        ));
        assert!(matches!(
            checked_addresses(vec![public, lan], permitted),
            Err(AddressCheckError::Rejected)
        ));
        // Deduplication keeps the order and folds the mapped IPv4 form back to IPv4.
        assert_eq!(
            checked_addresses(vec![public, mapped_lan, public], |address| matches!(
                canonical_ip(address),
                IpAddr::V4(_)
            ))
            .expect("an all-permitted answer is pin-ready"),
            vec![public, lan]
        );
    }

    #[test]
    fn resolver_overrides_cover_both_root_dot_spellings() {
        assert_eq!(
            resolver_override_hosts("media.example."),
            ("media.example.", Some("media.example"))
        );
        assert_eq!(
            resolver_override_hosts("media.example"),
            ("media.example", None)
        );
        // Nothing to fall back to when the authority is the root label itself.
        assert_eq!(resolver_override_hosts("."), (".", None));
    }
}
