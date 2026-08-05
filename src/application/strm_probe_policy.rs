use std::net::IpAddr;

use reqwest::Url;

pub fn validate_remote_media_url(value: &str) -> bool {
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
        || host.parse::<IpAddr>().is_ok_and(is_private_or_reserved))
}

fn is_private_or_reserved(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_multicast()
                || (address.octets()[0] == 100 && (64..=127).contains(&address.octets()[1]))
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
