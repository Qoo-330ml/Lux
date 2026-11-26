use std::{
    collections::VecDeque,
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use reqwest::{
    Client, ClientBuilder, Proxy, Url,
    header::{ACCEPT_ENCODING, CONTENT_RANGE, CONTENT_TYPE, ETAG, LOCATION, RANGE, USER_AGENT},
};

#[cfg(test)]
use crate::application::remote_url_policy::checked_addresses;
use crate::{
    application::remote_url_policy::{
        AddressCheckError, HostResolutionError, HostResolver, OutboundHost, SystemHostResolver,
        canonical_ip, domain_matches_any, ipv4_is_6to4_relay_anycast, ipv4_is_benchmark,
        ipv4_is_documentation, ipv4_is_ietf_protocol_assignment, ipv4_is_reserved,
        ipv4_is_this_network, ipv6_is_discard_only, ipv6_is_documentation, ipv6_is_tunnel,
        pin_resolved_addresses, resolve_checked_addresses,
    },
    network::{NetworkProxyError, normalize_proxy_url},
};

const DEFAULT_USER_AGENT: &str = "Lux/strm-playback";
const MAX_REDIRECTS: usize = 8;
const MAX_URL_CHARS: usize = 8 * 1024;
/// Upper bound for pinned clients kept for connection reuse. Players issue frequent Range seeks
/// against the same upstream, so a small cache keeps their TCP/TLS connections alive without
/// letting a long-lived resolver accumulate clients.
const CLIENT_CACHE_CAPACITY: usize = 32;
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
pub type StrmHostLookupFuture<'a> = crate::application::remote_url_policy::HostLookupFuture<'a>;

type HostLookup = Arc<dyn HostResolver>;

#[derive(Clone)]
pub struct StrmPlaybackResolver {
    egress: StrmEgress,
}

/// How a resolver reaches the upstream host.
#[derive(Clone)]
enum StrmEgress {
    /// Production direct connections. Every host name is resolved, the whole answer is checked,
    /// every checked address is pinned into the client that issues that hop, and those clients are
    /// pooled so repeated Range seeks reuse their TCP/TLS connections.
    Direct {
        lookup: HostLookup,
        literal_client: Client,
        clients: PinnedClients,
    },
    /// Explicit test HTTP proxy. The proxy, not this process, performs the host name resolution,
    /// so the direct-path DNS checks are skipped while the literal/name blacklist still applies.
    Proxy(Client),
}

/// Bounded LRU cache of pinned clients, keyed by the normalized checked address set they may
/// connect to.
#[derive(Clone, Default)]
struct PinnedClients {
    inner: Arc<Mutex<PinnedClientCache>>,
}

#[derive(Default)]
struct PinnedClientCache {
    entries: VecDeque<(PinnedClientKey, Client)>,
}

/// The resolution result a client was pinned to. A changed address set is a different key, so a
/// rebinding answer can never reuse a client that still trusts the previous answer.
#[derive(Clone, PartialEq, Eq)]
struct PinnedClientKey {
    scheme: String,
    host: String,
    port: u16,
    addresses: Vec<SocketAddr>,
}

impl PinnedClients {
    fn get(&self, key: &PinnedClientKey) -> Option<Client> {
        let mut cache = self.lock();
        let index = cache.entries.iter().position(|(cached, _)| cached == key)?;
        let entry = cache.entries.remove(index)?;
        let client = entry.1.clone();
        cache.entries.push_back(entry);
        Some(client)
    }

    fn insert(&self, key: PinnedClientKey, client: Client) {
        let mut cache = self.lock();
        if let Some(index) = cache.entries.iter().position(|(cached, _)| cached == &key) {
            cache.entries.remove(index);
        } else if cache.entries.len() >= CLIENT_CACHE_CAPACITY {
            cache.entries.pop_front();
        }
        cache.entries.push_back((key, client));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().entries.len()
    }

    fn lock(&self) -> MutexGuard<'_, PinnedClientCache> {
        // A poisoned lock only means an earlier holder panicked; the queue itself stays usable.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
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
                lookup: Arc::new(SystemHostResolver),
                literal_client: build_direct_literal_client()?,
                clients: PinnedClients::default(),
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
                literal_client: build_direct_literal_client()?,
                clients: PinnedClients::default(),
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

    /// Builds (or reuses) the client for one hop.
    ///
    /// A host name is resolved exactly once here and every returned address is checked. All
    /// checked addresses are pinned through [`pin_resolved_addresses`] so a second DNS answer can
    /// never redirect the connection, while the client may still fall back between the verified
    /// addresses (dual-stack hosts, one dead peer). Clients are pooled by
    /// `(scheme, host, port, checked addresses)` so repeated Range seeks reuse connections. IP
    /// literals need no lookup.
    async fn client_for(&self, url: &Url) -> Result<Client, StrmPlaybackError> {
        match &self.egress {
            StrmEgress::Proxy(client) => Ok(client.clone()),
            StrmEgress::Direct {
                lookup,
                literal_client,
                clients,
            } => {
                let host = OutboundHost::from_url(url).ok_or(StrmPlaybackError::InvalidUrl)?;
                let port = url
                    .port_or_known_default()
                    .ok_or(StrmPlaybackError::InvalidUrl)?;
                let Some(request_host) = host.request_host() else {
                    // A literal cannot be redirected by DNS, so there is nothing to pin.
                    return Ok(literal_client.clone());
                };
                let addresses =
                    resolve_checked_addresses(lookup.as_ref(), request_host, port, |address| {
                        !is_disallowed_address(address)
                    })
                    .await
                    .map_err(resolution_error)?;
                let mut key_addresses = addresses.clone();
                key_addresses.sort_unstable();
                key_addresses.dedup();
                let key = PinnedClientKey {
                    scheme: url.scheme().to_owned(),
                    host: request_host.to_owned(),
                    port,
                    addresses: key_addresses,
                };
                if let Some(client) = clients.get(&key) {
                    return Ok(client);
                }
                let builder = pin_resolved_addresses(
                    base_client_builder().no_proxy(),
                    request_host,
                    &addresses,
                );
                let client = builder
                    .build()
                    .map_err(|error| StrmPlaybackError::ClientBuild(error.to_string()))?;
                clients.insert(key, client.clone());
                Ok(client)
            }
        }
    }

    #[cfg(test)]
    fn cached_pinned_client_count(&self) -> usize {
        match &self.egress {
            StrmEgress::Direct { clients, .. } => clients.len(),
            StrmEgress::Proxy(_) => 0,
        }
    }
}

fn build_direct_literal_client() -> Result<Client, StrmPlaybackError> {
    base_client_builder()
        .no_proxy()
        .build()
        .map_err(|error| StrmPlaybackError::ClientBuild(error.to_string()))
}

fn base_client_builder() -> ClientBuilder {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
}

/// Maps a resolution failure onto the STRM error contract.
fn resolution_error(error: HostResolutionError) -> StrmPlaybackError {
    match error {
        HostResolutionError::Lookup(_) => StrmPlaybackError::HostLookupFailed,
        HostResolutionError::Check(AddressCheckError::Empty) => StrmPlaybackError::HostLookupFailed,
        HostResolutionError::Check(AddressCheckError::Rejected) => StrmPlaybackError::InvalidUrl,
    }
}

/// Checks a complete resolver answer and returns every address that may be pinned.
#[cfg(test)]
fn pinned_addresses(candidates: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, StrmPlaybackError> {
    checked_addresses(candidates, |address| !is_disallowed_address(address))
        .map_err(|error| resolution_error(HostResolutionError::Check(error)))
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
    match OutboundHost::from_url(&url) {
        Some(OutboundHost::Domain { normalized, .. }) => {
            // A trailing root dot (`localhost.`) is the same name to DNS, so the shared host type
            // has already normalized it before the blacklist comparison.
            if is_blocked_domain(&normalized) {
                return Err(StrmPlaybackError::InvalidUrl);
            }
        }
        Some(OutboundHost::Ipv4(address)) => {
            if is_disallowed_ipv4(address) {
                return Err(StrmPlaybackError::InvalidUrl);
            }
        }
        Some(OutboundHost::Ipv6(address)) => {
            if is_disallowed_address(IpAddr::V6(address)) {
                return Err(StrmPlaybackError::InvalidUrl);
            }
        }
        None => return Err(StrmPlaybackError::InvalidUrl),
    }
    Ok(url)
}

fn is_blocked_domain(host: &str) -> bool {
    const BLOCKED: [&str; 2] = ["localhost", "metadata.google.internal"];
    domain_matches_any(host, &BLOCKED)
}

fn is_disallowed_address(address: IpAddr) -> bool {
    match canonical_ip(address) {
        IpAddr::V4(address) => is_disallowed_ipv4(address),
        IpAddr::V6(address) => is_disallowed_ipv6(address),
    }
}

/// The STRM policy deliberately keeps RFC1918 (`10/8`, `172.16/12`, `192.168/16`), ULA
/// (`fc00::/7`) and carrier-grade NAT (`100.64/10`) LAN playback targets allowed; self-hosted
/// setups reach their media sources through Tailscale or ZeroTier, which use `100.64.0.0/10`.
/// Everything that is not a routable unicast peer is rejected instead, so this must not be
/// replaced by a generic "block all private addresses" policy.
fn is_disallowed_ipv4(address: Ipv4Addr) -> bool {
    address.is_loopback()
        || address.is_link_local()
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_broadcast()
        || ipv4_is_this_network(address)
        // 240/4 is reserved for future use and never a unicast destination.
        || ipv4_is_reserved(address)
        // Special-purpose, documentation and benchmark blocks are not playable media hosts.
        || ipv4_is_ietf_protocol_assignment(address)
        || ipv4_is_documentation(address)
        || ipv4_is_6to4_relay_anycast(address)
        || ipv4_is_benchmark(address)
}

/// Keeps ULA (`fc00::/7`) reachable for LAN playback and rejects every tunnelled, non-unicast or
/// documentation range.
fn is_disallowed_ipv6(address: Ipv6Addr) -> bool {
    address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || address.is_unicast_link_local()
        // 6to4 and Teredo wrap arbitrary IPv4 peers; NAT64 reaches an embedded IPv4 target.
        || ipv6_is_tunnel(address)
        // 2001:db8::/32 is the IPv6 documentation range and never routable.
        || ipv6_is_documentation(address)
        // 100::/64 is discard-only.
        || ipv6_is_discard_only(address)
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
        sync::{Arc, Mutex},
    };

    use reqwest::Client;

    use super::{
        CLIENT_CACHE_CAPACITY, MAX_RANGE_BYTES, PinnedClientKey, PinnedClients,
        StrmHostLookupFuture, StrmPlaybackError, StrmPlaybackResolver, content_range_matches,
        is_disallowed_address, pinned_addresses, upstream_range, validate_range, validate_url,
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
            "http://192.0.2.7/video.mkv",
            "http://198.18.0.1/video.mkv",
            "http://198.51.100.9/video.mkv",
            "http://203.0.113.4/video.mkv",
            "http://[2002:7f00:1::1]/video.mkv",
            "http://[2001:0:4136:e378:8000:63bf:3fff:fdd2]/video.mkv",
            "http://[64:ff9b::7f00:1]/video.mkv",
            "http://[2001:db8::1]/video.mkv",
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
            // Tailscale / ZeroTier use the carrier-grade NAT range for self-hosted media sources.
            "http://100.64.0.1/video.mkv",
            "http://100.100.100.100:8096/video.mkv",
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
            "2001:db8::1",
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
            "100.64.0.1",
            "100.127.255.254",
            "93.184.216.34",
            "2606:4700:4700::1111",
        ] {
            let address: IpAddr = value.parse().expect("address should parse");
            assert!(!is_disallowed_address(address), "{value} must stay allowed");
        }
    }

    #[test]
    fn keeps_every_checked_address_and_unwraps_embedded_ipv4() {
        let global_v6 = SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0x2606, 0x4700, 0, 0, 0, 0, 0, 0x1111)),
            443,
        );
        let public_v4 = socket([93, 184, 216, 34], 443);
        let mapped_lan = SocketAddr::new(
            IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x0a00, 0x0005)),
            8080,
        );
        let lan_v4 = socket([10, 0, 0, 5], 8080);
        // Every checked candidate stays pinned so the client can still fall back between them.
        assert_eq!(
            pinned_addresses(vec![global_v6, public_v4]).expect("answer should be accepted"),
            vec![global_v6, public_v4]
        );
        // Embedded IPv4 forms are unwrapped, and duplicates collapse while order is preserved.
        assert_eq!(
            pinned_addresses(vec![mapped_lan, lan_v4, public_v4, lan_v4])
                .expect("mapped LAN address should be accepted"),
            vec![lan_v4, public_v4]
        );
        assert!(matches!(
            pinned_addresses(vec![public_v4, socket([127, 0, 0, 1], 443)]),
            Err(StrmPlaybackError::InvalidUrl)
        ));
        assert!(matches!(
            pinned_addresses(Vec::new()),
            Err(StrmPlaybackError::HostLookupFailed)
        ));
    }

    fn pinned_client_key(host: &str, addresses: Vec<SocketAddr>) -> PinnedClientKey {
        PinnedClientKey {
            scheme: "http".to_owned(),
            host: host.to_owned(),
            port: 80,
            addresses,
        }
    }

    fn test_client() -> Client {
        Client::builder()
            .no_proxy()
            .build()
            .expect("test client should build")
    }

    #[test]
    fn pinned_client_cache_reuses_clients_and_stays_bounded() {
        let clients = PinnedClients::default();
        let key = pinned_client_key("media.example.test", vec![socket([93, 184, 216, 34], 80)]);
        assert!(clients.get(&key).is_none());
        clients.insert(key.clone(), test_client());
        assert!(clients.get(&key).is_some());
        // A changed resolution answer never reuses a client pinned to the old addresses.
        assert!(
            clients
                .get(&pinned_client_key(
                    "media.example.test",
                    vec![socket([10, 0, 0, 5], 80)]
                ))
                .is_none()
        );

        for index in 0..CLIENT_CACHE_CAPACITY {
            clients.insert(
                pinned_client_key(&format!("host{index}.example.test"), Vec::new()),
                test_client(),
            );
        }
        assert_eq!(clients.len(), CLIENT_CACHE_CAPACITY);
        assert!(
            clients.get(&key).is_none(),
            "the oldest client should be evicted once the cache is full"
        );
        assert!(
            clients
                .get(&pinned_client_key(
                    &format!("host{}.example.test", CLIENT_CACHE_CAPACITY - 1),
                    Vec::new()
                ))
                .is_some()
        );
    }

    #[test]
    fn pinned_client_cache_evicts_least_recently_used_entry() {
        let clients = PinnedClients::default();
        let first = pinned_client_key("host0.example.test", vec![socket([93, 184, 216, 1], 80)]);
        let second = pinned_client_key("host1.example.test", vec![socket([93, 184, 216, 2], 80)]);
        clients.insert(first.clone(), test_client());
        clients.insert(second.clone(), test_client());
        for index in 2..CLIENT_CACHE_CAPACITY {
            clients.insert(
                pinned_client_key(
                    &format!("host{index}.example.test"),
                    vec![socket(
                        [93, 184, 216, u8::try_from(index + 1).expect("u8")],
                        80,
                    )],
                ),
                test_client(),
            );
        }
        // Touching `first` should promote it ahead of `second` so `second` is evicted next.
        assert!(clients.get(&first).is_some());
        clients.insert(
            pinned_client_key("newest.example.test", vec![socket([93, 184, 216, 200], 80)]),
            test_client(),
        );
        assert!(
            clients.get(&first).is_some(),
            "recently used client must stay cached"
        );
        assert!(
            clients.get(&second).is_none(),
            "least recently used client should be evicted when full"
        );
    }

    #[tokio::test]
    async fn client_for_reuses_rotated_dns_answers_and_shares_literal_client() {
        let call_index = Arc::new(Mutex::new(0_usize));
        let counter = Arc::clone(&call_index);
        let resolver = StrmPlaybackResolver::new_with_host_lookup_for_tests(
            move |_host: String, port: u16| -> StrmHostLookupFuture<'static> {
                let counter = Arc::clone(&counter);
                Box::pin(async move {
                    let mut guard = counter.lock().expect("lookup counter");
                    let current = *guard;
                    *guard += 1;
                    let a = socket([93, 184, 216, 34], port);
                    let b = socket([93, 184, 216, 35], port);
                    if current.is_multiple_of(2) {
                        Ok(vec![a, b])
                    } else {
                        Ok(vec![b, a])
                    }
                })
            },
        )
        .expect("resolver should build");

        let domain_url =
            reqwest::Url::parse("http://cdn.example.test/video.mkv").expect("valid url");
        let _ = resolver
            .client_for(&domain_url)
            .await
            .expect("first lookup");
        let _ = resolver
            .client_for(&domain_url)
            .await
            .expect("second lookup with rotated DNS order");
        assert_eq!(
            resolver.cached_pinned_client_count(),
            1,
            "rotated DNS answer order must reuse the same pinned client"
        );

        let literal_a = reqwest::Url::parse("http://10.0.0.5:8080/a.mkv").expect("valid literal");
        let literal_b = reqwest::Url::parse("http://10.0.0.6:8080/b.mkv").expect("valid literal");
        let _ = resolver
            .client_for(&literal_a)
            .await
            .expect("first literal");
        let _ = resolver
            .client_for(&literal_b)
            .await
            .expect("second literal");
        assert_eq!(
            resolver.cached_pinned_client_count(),
            1,
            "IP literals should reuse the shared direct client without occupying pinned slots"
        );
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
