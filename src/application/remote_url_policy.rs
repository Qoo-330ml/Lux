use std::net::{IpAddr, SocketAddr};

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

fn is_private_or_reserved(address: IpAddr) -> bool {
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
        }
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unspecified()
                || address.is_multicast()
                || address.is_unique_local()
                || address.is_unicast_link_local()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::validate_remote_media_url;

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
}
