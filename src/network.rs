use std::{env, fmt, net::IpAddr};

use reqwest::{Client, ClientBuilder, Proxy, Url};

pub const PROXY_URL_ENV: &str = "LUX_PROXY_URL";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkProxyError {
    InvalidUrl,
}

impl fmt::Display for NetworkProxyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl => {
                formatter.write_str("network proxy URL must be a valid http(s) URL")
            }
        }
    }
}

impl std::error::Error for NetworkProxyError {}

pub fn proxy_url_from_env() -> Result<Option<String>, NetworkProxyError> {
    match env::var(PROXY_URL_ENV) {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => normalize_proxy_url(&value).map(Some),
        Err(_) => Ok(None),
    }
}

pub fn client_builder_from_env() -> Result<ClientBuilder, NetworkProxyError> {
    apply_proxy(Client::builder(), proxy_url_from_env()?.as_deref())
}

pub fn apply_proxy(
    builder: ClientBuilder,
    proxy_url: Option<&str>,
) -> Result<ClientBuilder, NetworkProxyError> {
    let Some(proxy_url) = proxy_url.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(builder);
    };
    let proxy_url = normalize_proxy_url(proxy_url)?;
    let proxy = Proxy::all(proxy_url).map_err(|_| NetworkProxyError::InvalidUrl)?;
    Ok(builder.proxy(proxy))
}

fn normalize_proxy_url(value: &str) -> Result<String, NetworkProxyError> {
    let value = value.trim();
    let url = Url::parse(value).map_err(|_| NetworkProxyError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(NetworkProxyError::InvalidUrl);
    }
    Ok(value.to_owned())
}

#[derive(Clone, Debug, Default)]
pub struct RemoteAccessPolicy {
    trusted_proxies: Vec<IpCidr>,
}

impl RemoteAccessPolicy {
    pub fn from_cidrs<I, S>(cidrs: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let trusted_proxies = cidrs
            .into_iter()
            .map(|value| IpCidr::parse(value.as_ref().trim()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { trusted_proxies })
    }

    pub fn from_env() -> Self {
        let value = std::env::var("LUX_TRUSTED_PROXY_CIDRS").unwrap_or_default();
        Self::from_cidrs(value.split(',').filter(|value| !value.trim().is_empty()))
            .unwrap_or_default()
    }

    pub fn is_remote(&self, peer: Option<&str>, forwarded_for: Option<&str>) -> bool {
        let Some(peer) = peer.and_then(|value| value.parse::<IpAddr>().ok()) else {
            return false;
        };
        let client = if self.is_trusted_ip(peer) {
            forwarded_for.and_then(first_forwarded_ip).unwrap_or(peer)
        } else {
            peer
        };
        is_public_address(client)
    }

    pub fn is_trusted_proxy(&self, peer: Option<&str>) -> bool {
        peer.and_then(|value| value.parse::<IpAddr>().ok())
            .is_some_and(|peer| self.is_trusted_ip(peer))
    }

    pub fn is_secure_request(&self, peer: Option<&str>, forwarded_proto: Option<&str>) -> bool {
        self.is_trusted_proxy(peer)
            && forwarded_proto
                .and_then(first_forwarded_proto)
                .is_some_and(|value| value.eq_ignore_ascii_case("https"))
    }

    fn is_trusted_ip(&self, peer: IpAddr) -> bool {
        self.trusted_proxies
            .iter()
            .any(|proxy| proxy.contains(peer))
    }
}

#[derive(Clone, Copy, Debug)]
struct IpCidr {
    network: IpAddr,
    prefix: u8,
}

impl IpCidr {
    fn parse(value: &str) -> Result<Self, String> {
        let (address, prefix) = value
            .split_once('/')
            .ok_or_else(|| format!("CIDR must contain '/': {value}"))?;
        let network = address
            .parse::<IpAddr>()
            .map_err(|_| format!("CIDR address is invalid: {value}"))?;
        let max_prefix = match network {
            IpAddr::V4(_) => 32,
            IpAddr::V6(_) => 128,
        };
        let prefix = prefix
            .parse::<u8>()
            .map_err(|_| format!("CIDR prefix is invalid: {value}"))?;
        if prefix > max_prefix {
            return Err(format!("CIDR prefix is out of range: {value}"));
        }
        Ok(Self { network, prefix })
    }

    fn contains(self, address: IpAddr) -> bool {
        match (self.network, address) {
            (IpAddr::V4(network), IpAddr::V4(address)) => {
                let network = u32::from(network);
                let address = u32::from(address);
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.prefix)
                };
                network & mask == address & mask
            }
            (IpAddr::V6(network), IpAddr::V6(address)) => {
                let network = u128::from(network);
                let address = u128::from(address);
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - self.prefix)
                };
                network & mask == address & mask
            }
            _ => false,
        }
    }
}

fn first_forwarded_ip(value: &str) -> Option<IpAddr> {
    value
        .split(',')
        .next()
        .map(str::trim)
        .and_then(|value| value.parse::<IpAddr>().ok())
}

fn first_forwarded_proto(value: &str) -> Option<&str> {
    value
        .split(',')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn is_public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let value = u32::from(address);
            let first = value >> 24;
            let second = (value >> 16) & 0xff;
            !matches!(
                (first, second),
                (0, _)
                    | (10, _)
                    | (100, 64..=127)
                    | (127, _)
                    | (169, 254)
                    | (172, 16..=31)
                    | (192, 168)
            )
        }
        IpAddr::V6(address) => {
            let value = u128::from(address);
            address != std::net::Ipv6Addr::LOCALHOST
                && value >> 118 != 0b111111
                && (value >> 121 != 0b1111110)
                && (value >> 120 != 0b1111111010)
        }
    }
}

#[cfg(test)]
mod tests {
    use reqwest::Client;

    use super::{NetworkProxyError, apply_proxy};

    #[test]
    fn proxy_configuration_accepts_http_and_https_and_rejects_other_schemes() {
        assert!(apply_proxy(Client::builder(), Some("http://192.168.1.2:7890")).is_ok());
        assert!(apply_proxy(Client::builder(), Some("https://proxy.invalid:8443")).is_ok());
        assert!(matches!(
            apply_proxy(Client::builder(), Some("socks5://proxy.invalid:1080")),
            Err(NetworkProxyError::InvalidUrl)
        ));
    }

    #[test]
    fn empty_proxy_configuration_is_disabled() {
        assert!(apply_proxy(Client::builder(), Some("  ")).is_ok());
        assert!(apply_proxy(Client::builder(), None).is_ok());
    }
}
