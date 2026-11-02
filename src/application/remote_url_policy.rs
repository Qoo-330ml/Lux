use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::Url;
use tokio::net::lookup_host;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteMediaUrlError {
    Invalid,
    BlockedHost,
    ResolutionFailed,
}

pub async fn validate_and_resolve_remote_media_url(
    value: &str,
) -> Result<(Url, SocketAddr), RemoteMediaUrlError> {
    if !validate_remote_media_url(value) {
        return Err(RemoteMediaUrlError::Invalid);
    }
    let url = Url::parse(value).map_err(|_| RemoteMediaUrlError::Invalid)?;
    let host = url.host_str().ok_or(RemoteMediaUrlError::Invalid)?;
    let port = url
        .port_or_known_default()
        .ok_or(RemoteMediaUrlError::Invalid)?;
    let addresses = lookup_host((host, port))
        .await
        .map_err(|_| RemoteMediaUrlError::ResolutionFailed)?
        .collect::<Vec<_>>();
    let Some(address) = addresses.first().copied() else {
        return Err(RemoteMediaUrlError::ResolutionFailed);
    };
    if addresses
        .iter()
        .any(|address| is_private_or_reserved(address.ip()))
    {
        return Err(RemoteMediaUrlError::BlockedHost);
    }
    Ok((url, address))
}

fn validate_remote_media_url(value: &str) -> bool {
    if value.chars().count() > 8 * 1024 {
        return false;
    }
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    !(host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".localhost")
        || host.eq_ignore_ascii_case("metadata.google.internal")
        || host.ends_with(".metadata.google.internal")
        || host.parse::<IpAddr>().is_ok_and(is_private_or_reserved))
}

pub(crate) fn is_private_or_reserved(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let [first, second, third, _] = address.octets();
            address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_multicast()
                || matches!(
                    (first, second, third),
                    (0, _, _)
                        | (100, 64..=127, _)
                        | (192, 0, 0..=255)
                        | (192, 88, 99)
                        | (198, 18..=19, _)
                        | (198, 51, 100)
                        | (203, 0, 113)
                )
                // 240/4 reserved and the limited broadcast address.
                || first >= 240
        }
        IpAddr::V6(address) => {
            // IPv4-mapped addresses reach the embedded IPv4 target, so classify them as IPv4.
            if let Some(mapped) = address.to_ipv4_mapped() {
                return is_private_or_reserved(IpAddr::V4(mapped));
            }
            // The deprecated IPv4-compatible form (`::/96`) must not bypass the IPv4 policy.
            if let Some(compatible) = ipv4_compatible_target(address) {
                return is_private_or_reserved(IpAddr::V4(compatible));
            }
            let segments = address.segments();
            address.is_loopback()
                || address.is_unspecified()
                || address.is_multicast()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                // 6to4, Teredo, and NAT64 can wrap arbitrary IPv4 destinations; 2001:db8::/32 is
                // the IPv6 documentation range and never routable.
                || segments[0] == 0x2002
                || segments[0] == 0x2001 && segments[1] == 0
                || segments[0] == 0x2001 && segments[1] == 0x0db8
                || segments[0] == 0x0064 && segments[1] == 0xff9b
        }
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

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::{is_private_or_reserved, validate_remote_media_url};

    #[test]
    fn download_url_policy_rejects_credentials_fragments_and_private_hosts() {
        for value in [
            "http://user:password@example.com/video.mkv",
            "https://example.com/video.mkv#fragment",
            "http://127.0.0.1/video.mkv",
            "http://192.168.1.10/video.mkv",
            "http://metadata.google.internal/video.mkv",
        ] {
            assert!(!validate_remote_media_url(value), "URL should be rejected");
        }
    }

    #[test]
    fn download_url_policy_accepts_public_http_url_with_query() {
        assert!(validate_remote_media_url(
            "https://example.com/video.mkv?signature=fixture"
        ));
    }

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
}
