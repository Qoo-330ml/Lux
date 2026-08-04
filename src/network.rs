use std::net::IpAddr;

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
