use std::{
    fmt,
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use reqwest::{
    Client, ClientBuilder, Proxy, Url,
    header::{ACCEPT_ENCODING, CONTENT_RANGE, CONTENT_TYPE, ETAG, LOCATION, RANGE, USER_AGENT},
};
use url::Host;

use crate::{
    application::remote_url_policy::ipv4_compatible_target,
    network::{NetworkProxyError, normalize_proxy_url},
};

const DEFAULT_USER_AGENT: &str = "Lux/strm-playback";
const MAX_REDIRECTS: usize = 8;
const MAX_URL_CHARS: usize = 8 * 1024;
pub const MAX_RANGE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug)]
pub enum StrmPlaybackError {
    InvalidUrl,
    InvalidRedirect,
    MissingRedirectLocation,
    TooManyRedirects,
    UnsupportedStatus(u16),
    RequestFailed,
    HostLookupFailed,
    InvalidRange,
    MissingContentRange,
    ResponseTooLarge,
    ProxyConfiguration(NetworkProxyError),
    ClientBuild(String),
}

impl fmt::Display for StrmPlaybackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl => formatter.write_str("STRM playback URL is invalid"),
            Self::InvalidRedirect => formatter.write_str("STRM redirect URL is invalid"),
            Self::MissingRedirectLocation => {
                formatter.write_str("STRM redirect response has no Location")
            }
            Self::TooManyRedirects => {
                formatter.write_str("STRM playback redirected too many times")
            }
            Self::UnsupportedStatus(status) => {
                write!(
                    formatter,
                    "STRM playback returned unsupported HTTP status {status}"
                )
            }
            Self::RequestFailed => formatter.write_str("STRM playback request failed"),
            Self::HostLookupFailed => {
                formatter.write_str("STRM playback host could not be resolved")
            }
            Self::InvalidRange => formatter.write_str("STRM playback range is invalid"),
            Self::MissingContentRange => {
                formatter.write_str("STRM playback response has no Content-Range")
            }
            Self::ResponseTooLarge => {
                formatter.write_str("STRM playback range response is too large")
            }
            Self::ProxyConfiguration(error) => write!(formatter, "{error}"),
            Self::ClientBuild(error) => write!(formatter, "STRM playback client failed: {error}"),
        }
    }
}

impl std::error::Error for StrmPlaybackError {}

/// Future returned by a [`StrmPlaybackResolver`] host lookup.
pub type StrmHostLookupFuture<'a> =
    Pin<Box<dyn Future<Output = std::io::Result<Vec<SocketAddr>>> + Send + 'a>>;

type HostLookup = Arc<dyn Fn(String, u16) -> StrmHostLookupFuture<'static> + Send + Sync>;

#[derive(Clone)]
pub struct StrmPlaybackResolver {
    egress: StrmEgress,
}

/// How a resolver reaches the upstream host.
#[derive(Clone)]
enum StrmEgress {
    /// Production direct connections. Every host name is resolved, the whole answer is checked,
    /// and the chosen address is pinned into the client that issues that hop.
    Direct { lookup: HostLookup },
    /// Explicit test HTTP proxy. The proxy, not this process, performs the host name resolution,
    /// so the direct-path DNS checks are skipped while the literal/name blacklist still applies.
    Proxy(Client),
}

pub struct StrmRangeResponse {
    pub content_range: String,
    pub content_length: u64,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub body: Vec<u8>,
}

impl StrmPlaybackResolver {
    pub fn new() -> Result<Self, StrmPlaybackError> {
        Ok(Self {
            egress: StrmEgress::Direct {
                lookup: system_host_lookup(),
            },
        })
    }

    #[doc(hidden)]
    pub fn new_with_proxy_for_tests(proxy_url: String) -> Result<Self, StrmPlaybackError> {
        let proxy_url =
            normalize_proxy_url(&proxy_url).map_err(StrmPlaybackError::ProxyConfiguration)?;
        let proxy = Proxy::all(proxy_url)
            .map_err(|_| StrmPlaybackError::ProxyConfiguration(NetworkProxyError::InvalidUrl))?;
        let client = base_client_builder()
            .proxy(proxy)
            .build()
            .map_err(|error| StrmPlaybackError::ClientBuild(error.to_string()))?;
        Ok(Self {
            egress: StrmEgress::Proxy(client),
        })
    }

    /// Direct-path resolver with an injectable host lookup so tests can script DNS answers,
    /// including mixed and rebinding responses, without touching the system resolver.
    #[doc(hidden)]
    pub fn new_with_host_lookup_for_tests<F>(lookup: F) -> Result<Self, StrmPlaybackError>
    where
        F: Fn(String, u16) -> StrmHostLookupFuture<'static> + Send + Sync + 'static,
    {
        Ok(Self {
            egress: StrmEgress::Direct {
                lookup: Arc::new(lookup),
            },
        })
    }

    pub async fn resolve(
        &self,
        target: &str,
        user_agent: Option<&str>,
    ) -> Result<Url, StrmPlaybackError> {
        let user_agent = user_agent.unwrap_or(DEFAULT_USER_AGENT);
        let (url, response) = self
            .send_following_redirects(target, "bytes=0-0", false, user_agent)
            .await?;
        let status = response.status();
        if status == reqwest::StatusCode::OK || status == reqwest::StatusCode::PARTIAL_CONTENT {
            return Ok(url);
        }
        Err(StrmPlaybackError::UnsupportedStatus(status.as_u16()))
    }

    pub async fn fetch_range(
        &self,
        target: &str,
        range: &str,
        user_agent: Option<&str>,
    ) -> Result<StrmRangeResponse, StrmPlaybackError> {
        validate_range(range)?;
        let user_agent = user_agent.unwrap_or(DEFAULT_USER_AGENT);
        let (_, mut response) = self
            .send_following_redirects(target, &upstream_range(range), true, user_agent)
            .await?;
        let status = response.status();
        if status != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(StrmPlaybackError::UnsupportedStatus(status.as_u16()));
        }
        let content_range = response
            .headers()
            .get(CONTENT_RANGE)
            .ok_or(StrmPlaybackError::MissingContentRange)?
            .to_str()
            .map_err(|_| StrmPlaybackError::MissingContentRange)?
            .to_owned();
        if !content_range_matches(&content_range, range) {
            return Err(StrmPlaybackError::MissingContentRange);
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let declared_content_length = response.content_length();
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| StrmPlaybackError::RequestFailed)?
        {
            if body.len() as u64 + chunk.len() as u64 > MAX_RANGE_BYTES {
                return Err(StrmPlaybackError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        let content_length = declared_content_length.unwrap_or(body.len() as u64);
        if content_length > MAX_RANGE_BYTES {
            return Err(StrmPlaybackError::ResponseTooLarge);
        }
        if body.len() as u64 != content_length {
            return Err(StrmPlaybackError::ResponseTooLarge);
        }
        Ok(StrmRangeResponse {
            content_range,
            content_length,
            content_type,
            etag,
            body,
        })
    }

    /// Follows redirects manually so every hop is validated and pinned before it is requested.
    async fn send_following_redirects(
        &self,
        target: &str,
        range_header: &str,
        identity_encoding: bool,
        user_agent: &str,
    ) -> Result<(Url, reqwest::Response), StrmPlaybackError> {
        let mut current = validate_url(target)?;
        for _ in 0..=MAX_REDIRECTS {
            let client = self.client_for(&current).await?;
            let mut request = client
                .get(current.clone())
                .header(RANGE, range_header)
                .header(USER_AGENT, user_agent);
            if identity_encoding {
                request = request.header(ACCEPT_ENCODING, "identity");
            }
            let response = request
                .send()
                .await
                .map_err(|_| StrmPlaybackError::RequestFailed)?;
            let status = response.status();
            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(LOCATION)
                    .ok_or(StrmPlaybackError::MissingRedirectLocation)?
                    .to_str()
                    .map_err(|_| StrmPlaybackError::InvalidRedirect)?;
                let next = current
                    .join(location)
                    .map_err(|_| StrmPlaybackError::InvalidRedirect)?;
                current = validate_url(next.as_str())?;
                continue;
            }
            return Ok((current, response));
        }
        Err(StrmPlaybackError::TooManyRedirects)
    }

    /// Builds the client for one hop.
    ///
    /// A host name is resolved exactly once here, every returned address is checked, and the
    /// selected address is pinned through [`ClientBuilder::resolve_to_addrs`] so the connection
    /// cannot be re-routed by a second DNS answer. IP literals need no lookup.
    async fn client_for(&self, url: &Url) -> Result<Client, StrmPlaybackError> {
        match &self.egress {
            StrmEgress::Proxy(client) => Ok(client.clone()),
            StrmEgress::Direct { lookup } => {
                let mut builder = base_client_builder().no_proxy();
                match url.host() {
                    Some(Host::Domain(domain)) => {
                        let host = url.host_str().unwrap_or(domain);
                        let port = url
                            .port_or_known_default()
                            .ok_or(StrmPlaybackError::InvalidUrl)?;
                        let candidates = lookup(host.to_owned(), port)
                            .await
                            .map_err(|_| StrmPlaybackError::HostLookupFailed)?;
                        let pinned = pinned_address(candidates)?;
                        builder = builder.resolve_to_addrs(host, &[pinned]);
                        // A trailing root dot names the same host, but the resolver override is
                        // keyed by the literal authority. Pin both spellings so neither one can
                        // fall back to a fresh system lookup.
                        let without_root_dot = host.trim_end_matches('.');
                        if without_root_dot.len() != host.len() && !without_root_dot.is_empty() {
                            builder = builder.resolve_to_addrs(without_root_dot, &[pinned]);
                        }
                    }
                    Some(Host::Ipv4(_) | Host::Ipv6(_)) => {}
                    None => return Err(StrmPlaybackError::InvalidUrl),
                }
                builder
                    .build()
                    .map_err(|error| StrmPlaybackError::ClientBuild(error.to_string()))
            }
        }
    }
}

fn base_client_builder() -> ClientBuilder {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
}

fn system_host_lookup() -> HostLookup {
    Arc::new(|host: String, port: u16| {
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host.as_str(), port)).await?;
            Ok(addresses.collect())
        })
    })
}

/// Checks a complete resolver answer and returns the address to pin.
///
/// A single forbidden candidate rejects the whole answer: otherwise an attacker could race a
/// mixed response or rebind between the check and the connection.
fn pinned_address(candidates: Vec<SocketAddr>) -> Result<SocketAddr, StrmPlaybackError> {
    let Some(first) = candidates.first().copied() else {
        return Err(StrmPlaybackError::HostLookupFailed);
    };
    if candidates
        .iter()
        .any(|candidate| is_disallowed_address(candidate.ip()))
    {
        return Err(StrmPlaybackError::InvalidUrl);
    }
    Ok(unwrap_embedded_ipv4(first))
}

/// Maps an IPv4-mapped or IPv4-compatible IPv6 socket back to its IPv4 target.
fn unwrap_embedded_ipv4(address: SocketAddr) -> SocketAddr {
    match address.ip() {
        IpAddr::V6(ipv6) => ipv6
            .to_ipv4_mapped()
            .or_else(|| ipv4_compatible_target(ipv6))
            .map_or(address, |ipv4| {
                SocketAddr::new(IpAddr::V4(ipv4), address.port())
            }),
        IpAddr::V4(_) => address,
    }
}

fn validate_range(value: &str) -> Result<(), StrmPlaybackError> {
    let Some(specification) = value.trim().strip_prefix("bytes=") else {
        return Err(StrmPlaybackError::InvalidRange);
    };
    if specification.contains(',') {
        return Err(StrmPlaybackError::InvalidRange);
    }
    let Some((start, end)) = specification.split_once('-') else {
        return Err(StrmPlaybackError::InvalidRange);
    };
    let Ok(start) = start.parse::<u64>() else {
        return Err(StrmPlaybackError::InvalidRange);
    };
    if end.is_empty() {
        return Ok(());
    }
    let Ok(end) = end.parse::<u64>() else {
        return Err(StrmPlaybackError::InvalidRange);
    };
    if start > end || end - start + 1 > MAX_RANGE_BYTES {
        return Err(StrmPlaybackError::InvalidRange);
    }
    Ok(())
}

fn upstream_range(value: &str) -> String {
    let Some((start, end)) = value
        .trim()
        .strip_prefix("bytes=")
        .and_then(|value| value.split_once('-'))
    else {
        return value.to_owned();
    };
    if end.is_empty() {
        let Ok(start) = start.parse::<u64>() else {
            return value.to_owned();
        };
        return format!(
            "bytes={start}-{}",
            start.saturating_add(MAX_RANGE_BYTES - 1)
        );
    }
    value.to_owned()
}

fn content_range_matches(value: &str, requested: &str) -> bool {
    let Some((requested_start, requested_end)) = requested
        .trim()
        .strip_prefix("bytes=")
        .and_then(|value| value.split_once('-'))
        .and_then(|(start, end)| {
            Some((
                start.parse::<u64>().ok()?,
                (!end.is_empty()).then(|| end.parse::<u64>().ok()).flatten(),
            ))
        })
    else {
        return false;
    };
    let Some((range, total)) = value
        .trim()
        .strip_prefix("bytes ")
        .and_then(|value| value.split_once('/'))
    else {
        return false;
    };
    let Some((start, end)) = range
        .split_once('-')
        .and_then(|(start, end)| Some((start.parse::<u64>().ok()?, end.parse::<u64>().ok()?)))
    else {
        return false;
    };
    let Some(total) = total.parse::<u64>().ok() else {
        return false;
    };
    start == requested_start
        && end >= requested_start
        && requested_end.is_none_or(|requested_end| end <= requested_end)
        && total > end
}

fn validate_url(value: &str) -> Result<Url, StrmPlaybackError> {
    if value.chars().count() > MAX_URL_CHARS {
        return Err(StrmPlaybackError::InvalidUrl);
    }
    let url = Url::parse(value).map_err(|_| StrmPlaybackError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(StrmPlaybackError::InvalidUrl);
    }
    match url.host() {
        Some(Host::Domain(domain)) => {
            // A trailing root dot (`localhost.`) is the same name to DNS, so normalize before the
            // blacklist comparison instead of comparing the raw authority.
            let normalized = normalized_domain(domain).ok_or(StrmPlaybackError::InvalidUrl)?;
            if is_blocked_domain(&normalized) {
                return Err(StrmPlaybackError::InvalidUrl);
            }
        }
        Some(Host::Ipv4(address)) => {
            if is_disallowed_ipv4(address) {
                return Err(StrmPlaybackError::InvalidUrl);
            }
        }
        Some(Host::Ipv6(address)) => {
            if is_disallowed_ipv6(address) {
                return Err(StrmPlaybackError::InvalidUrl);
            }
        }
        None => return Err(StrmPlaybackError::InvalidUrl),
    }
    Ok(url)
}

/// Lowercases a domain and strips the trailing root dot; an empty result is not a valid host.
fn normalized_domain(domain: &str) -> Option<String> {
    let trimmed = domain.trim_end_matches('.');
    (!trimmed.is_empty()).then(|| trimmed.to_ascii_lowercase())
}

fn is_blocked_domain(host: &str) -> bool {
    const BLOCKED: [&str; 2] = ["localhost", "metadata.google.internal"];
    BLOCKED.iter().any(|blocked| {
        host == *blocked
            || host
                .strip_suffix(blocked)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn is_disallowed_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_disallowed_ipv4(address),
        IpAddr::V6(address) => is_disallowed_ipv6(address),
    }
}

/// The STRM policy deliberately keeps RFC1918 (`10/8`, `172.16/12`, `192.168/16`) and ULA
/// (`fc00::/7`) LAN playback targets allowed. Everything that is not a routable unicast peer is
/// rejected instead, so this must not be replaced by a generic "block all private addresses"
/// policy.
fn is_disallowed_ipv4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    address.is_loopback()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_broadcast()
        || first == 0
        // 240/4 is reserved for future use and never a unicast destination.
        || first >= 240
        // Carrier-grade NAT reaches shared operator hosts instead of the user's LAN.
        || (first == 100 && (64..=127).contains(&second))
        // Special-purpose, documentation and benchmark blocks are not playable media hosts.
        || (first == 192 && second == 0 && matches!(third, 0 | 2))
        || (first == 192 && second == 88 && third == 99)
        || (first == 198 && matches!(second, 18 | 19))
        || (first == 198 && second == 51 && third == 100)
        || (first == 203 && second == 0 && third == 113)
}

fn is_disallowed_ipv6(address: Ipv6Addr) -> bool {
    // Mapped and deprecated IPv4-compatible forms reach an IPv4 destination, so unwrap first:
    // otherwise a tunnel could smuggle a loopback or link-local target past the IPv4 policy.
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_disallowed_ipv4(mapped);
    }
    if let Some(compatible) = ipv4_compatible_target(address) {
        return is_disallowed_ipv4(compatible);
    }
    let segments = address.segments();
    address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_unicast_link_local()
        // 6to4 and Teredo wrap arbitrary IPv4 peers; NAT64 reaches an embedded IPv4 target.
        || segments[0] == 0x2002
        || (segments[0] == 0x2001 && segments[1] == 0)
        || (segments[0] == 0x0064 && segments[1] == 0xff9b)
        // 100::/64 is discard-only.
        || (segments[0] == 0x0100 && segments[1] == 0 && segments[2] == 0 && segments[3] == 0)
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
        sync::{Arc, Mutex},
    };

    use super::{
        MAX_RANGE_BYTES, StrmHostLookupFuture, StrmPlaybackError, StrmPlaybackResolver,
        content_range_matches, is_disallowed_address, pinned_address, upstream_range,
        validate_range, validate_url,
    };

    fn lookup_returning(
        addresses: Vec<SocketAddr>,
    ) -> impl Fn(String, u16) -> StrmHostLookupFuture<'static> + Send + Sync + 'static {
        move |_host, _port| {
            let addresses = addresses.clone();
            Box::pin(async move { Ok(addresses) })
        }
    }

    fn socket(address: [u8; 4], port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::from(address)), port)
    }

    #[test]
    fn rejects_ipv6_loopback_mapped_and_link_local_literals() {
        for value in [
            "http://[::1]:8080/video.mkv",
            "http://[::ffff:127.0.0.1]:8080/video.mkv",
            "http://[::127.0.0.1]:8080/video.mkv",
            "http://[::ffff:169.254.169.254]/latest",
            "http://[fe80::1]/",
            "http://[::]/",
            "http://[ff02::1]/",
        ] {
            assert!(
                validate_url(value).is_err(),
                "URL must be rejected: {value}"
            );
        }
    }

    #[test]
    fn rejects_trailing_dot_local_and_metadata_hosts() {
        for value in [
            "http://localhost./video.mkv",
            "http://LOCALHOST.:8080/video.mkv",
            "http://sub.localhost./video.mkv",
            "http://metadata.google.internal./computeMetadata/v1/",
            "http://vm.metadata.google.internal./computeMetadata/v1/",
            "http://./video.mkv",
        ] {
            assert!(
                validate_url(value).is_err(),
                "URL must be rejected: {value}"
            );
        }
    }

    #[test]
    fn rejects_non_unicast_reserved_and_tunnelled_targets() {
        for value in [
            "http://127.0.0.1:8080/video.mkv",
            "http://169.254.169.254/latest/meta-data",
            "http://0.0.0.0:8080/video.mkv",
            "http://0.1.2.3/video.mkv",
            "http://224.0.0.1/video.mkv",
            "http://255.255.255.255/video.mkv",
            "http://240.0.0.1/video.mkv",
            "http://100.64.0.1/video.mkv",
            "http://192.0.2.7/video.mkv",
            "http://198.18.0.1/video.mkv",
            "http://198.51.100.9/video.mkv",
            "http://203.0.113.4/video.mkv",
            "http://[2002:7f00:1::1]/video.mkv",
            "http://[2001:0:4136:e378:8000:63bf:3fff:fdd2]/video.mkv",
            "http://[64:ff9b::7f00:1]/video.mkv",
            "http://[::127.0.0.1]/video.mkv",
        ] {
            assert!(
                validate_url(value).is_err(),
                "URL must be rejected: {value}"
            );
        }
    }

    #[test]
    fn keeps_lan_and_public_playback_targets_allowed() {
        for value in [
            "http://192.168.10.50:2083/custom/resolve?id=1",
            "http://10.0.0.5/video.mkv",
            "http://172.16.5.9:8080/video.mkv",
            "http://172.31.255.254/video.mkv",
            "http://[fd00::1]:8080/video.mkv",
            "http://[::ffff:10.0.0.5]/video.mkv",
            "http://[::10.0.0.5]/video.mkv",
            "https://93.184.216.34/video.mkv",
            "https://media.example.test/video.mkv",
        ] {
            assert!(
                validate_url(value).is_ok(),
                "URL must stay allowed: {value}"
            );
        }
    }

    #[test]
    fn classifies_embedded_and_tunnelled_addresses() {
        for value in [
            "::1",
            "::",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "::169.254.169.254",
            "fe80::1",
            "64:ff9b::7f00:1",
            "2002:7f00:1::",
            "2001:0::1",
            "100::1",
            "255.255.255.255",
        ] {
            let address: IpAddr = value.parse().expect("address should parse");
            assert!(is_disallowed_address(address), "{value} must be disallowed");
        }
        for value in [
            "::ffff:10.0.0.5",
            "::10.0.0.5",
            "fd00::1",
            "192.168.10.50",
            "10.0.0.5",
            "172.16.0.1",
            "93.184.216.34",
            "2606:4700:4700::1111",
        ] {
            let address: IpAddr = value.parse().expect("address should parse");
            assert!(!is_disallowed_address(address), "{value} must stay allowed");
        }
    }

    #[test]
    fn pins_one_checked_address_and_unwraps_embedded_ipv4() {
        let global_v6 = SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0, 0, 0, 0, 0, 0x1111)),
            443,
        );
        let public_v4 = socket([93, 184, 216, 34], 443);
        // The first checked candidate is pinned; no second resolution can replace it.
        assert_eq!(
            pinned_address(vec![global_v6, public_v4]).expect("answer should be accepted"),
            global_v6
        );
        assert_eq!(
            pinned_address(vec![SocketAddr::new(
                IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0a00, 0x0005)),
                8080,
            )])
            .expect("mapped LAN address should be accepted"),
            socket([10, 0, 0, 5], 8080)
        );
        assert!(matches!(
            pinned_address(vec![public_v4, socket([127, 0, 0, 1], 443)]),
            Err(StrmPlaybackError::InvalidUrl)
        ));
        assert!(matches!(
            pinned_address(Vec::new()),
            Err(StrmPlaybackError::HostLookupFailed)
        ));
    }

    #[tokio::test]
    async fn rejects_a_domain_when_any_resolved_address_is_disallowed() {
        let resolver =
            StrmPlaybackResolver::new_with_host_lookup_for_tests(lookup_returning(vec![
                socket([93, 184, 216, 34], 80),
                socket([127, 0, 0, 1], 80),
            ]))
            .expect("resolver should build");
        let error = resolver
            .resolve("http://media.example.test/video.mkv", None)
            .await
            .expect_err("a mixed answer must be rejected");
        assert!(matches!(error, StrmPlaybackError::InvalidUrl));
    }

    #[tokio::test]
    async fn rejects_a_domain_that_resolves_to_loopback_or_link_local() {
        for address in [socket([127, 0, 0, 1], 80), socket([169, 254, 169, 254], 80)] {
            let resolver =
                StrmPlaybackResolver::new_with_host_lookup_for_tests(lookup_returning(vec![
                    address,
                ]))
                .expect("resolver should build");
            let error = resolver
                .fetch_range("http://media.example.test/video.mkv", "bytes=0-3", None)
                .await
                .err()
                .expect("a forbidden resolution must be rejected");
            assert!(matches!(error, StrmPlaybackError::InvalidUrl));
        }
    }

    #[tokio::test]
    async fn rejects_empty_or_failed_resolutions() {
        let empty =
            StrmPlaybackResolver::new_with_host_lookup_for_tests(lookup_returning(Vec::new()))
                .expect("resolver should build");
        assert!(matches!(
            empty
                .resolve("http://media.example.test/video.mkv", None)
                .await,
            Err(StrmPlaybackError::HostLookupFailed)
        ));

        let failing = StrmPlaybackResolver::new_with_host_lookup_for_tests(
            |_host: String, _port: u16| -> StrmHostLookupFuture<'static> {
                Box::pin(async {
                    Err::<Vec<SocketAddr>, _>(std::io::Error::other("lookup failed"))
                })
            },
        )
        .expect("resolver should build");
        assert!(matches!(
            failing
                .resolve("http://media.example.test/video.mkv", None)
                .await,
            Err(StrmPlaybackError::HostLookupFailed)
        ));
    }

    #[tokio::test]
    async fn resolves_each_domain_only_once_per_hop() {
        let calls = Arc::new(Mutex::new(0_usize));
        let counter = Arc::clone(&calls);
        let resolver = StrmPlaybackResolver::new_with_host_lookup_for_tests(
            move |_host: String, port: u16| -> StrmHostLookupFuture<'static> {
                let counter = Arc::clone(&counter);
                Box::pin(async move {
                    *counter.lock().expect("lookup counter") += 1;
                    // Port zero is never connectable, so the request fails immediately after the
                    // single lookup this test observes.
                    Ok(vec![socket([10, 255, 255, 1], port)])
                })
            },
        )
        .expect("resolver should build");
        assert!(
            resolver
                .resolve("http://media.example.test:0/video.mkv", None)
                .await
                .is_err()
        );
        assert_eq!(*calls.lock().expect("lookup counter"), 1);
    }

    #[test]
    fn accepts_bounded_and_open_ended_ranges() {
        assert!(validate_range("bytes=0-1023").is_ok());
        assert!(validate_range(&format!("bytes=0-{}", MAX_RANGE_BYTES - 1)).is_ok());
        assert!(validate_range("bytes=0-").is_ok());
        assert!(validate_range("bytes=1048576-").is_ok());
        assert!(validate_range("bytes=0-1,4-5").is_err());
        assert!(validate_range(&format!("bytes=0-{}", MAX_RANGE_BYTES)).is_err());
    }

    #[test]
    fn open_ended_ranges_are_bounded_before_the_upstream_request() {
        assert_eq!(
            upstream_range("bytes=10-"),
            format!("bytes=10-{}", 10 + MAX_RANGE_BYTES - 1)
        );
        assert_eq!(upstream_range("bytes=10-20"), "bytes=10-20");
    }

    #[test]
    fn requires_the_upstream_content_range_to_match() {
        assert!(content_range_matches("bytes 0-99/1000", "bytes=0-99"));
        assert!(content_range_matches("bytes 0-49/50", "bytes=0-99"));
        assert!(content_range_matches("bytes 0-49/50", "bytes=0-"));
        assert!(!content_range_matches("bytes 1-99/1000", "bytes=0-99"));
        assert!(!content_range_matches("bytes 0-99/*", "bytes=0-99"));
    }
}
