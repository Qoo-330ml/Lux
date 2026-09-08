use std::{
    env, fmt, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    str,
};

use serde::{Deserialize, Serialize};
use tokio::{net::UdpSocket, sync::watch};
use url::Url;

use crate::storage::Database;

pub const DISCOVERY_PORT: u16 = 7359;

const DEFAULT_DISCOVERY_BIND_ADDR: &str = "0.0.0.0:7359";
const DISCOVERY_BIND_ADDR_ENV: &str = "LUX_DISCOVERY_BIND_ADDR";
const DISCOVERY_ADVERTISE_URL_ENV: &str = "LUX_DISCOVERY_ADVERTISE_URL";
const MAX_ADVERTISE_URL_LENGTH: usize = 2048;
const MAX_SERVER_NAME_LENGTH: usize = 256;
const MAX_SERVER_ID_LENGTH: usize = 256;
const MAX_DISCOVERY_DATAGRAM_BYTES: usize = 8192;
const DISCOVERY_REQUEST: &str = "who is embyserver?";
const DEFAULT_SERVER_NAME: &str = "Lux Server";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryConfig {
    pub bind_addr: SocketAddr,
    pub http_addr: SocketAddr,
    pub advertise_url: Option<String>,
}

impl DiscoveryConfig {
    pub fn new(
        bind_addr: SocketAddr,
        http_addr: SocketAddr,
        advertise_url: Option<String>,
    ) -> Result<Self, DiscoveryConfigError> {
        let advertise_url = advertise_url
            .as_deref()
            .map(normalize_advertise_url)
            .transpose()?;
        Ok(Self {
            bind_addr,
            http_addr,
            advertise_url,
        })
    }

    pub fn from_env(http_addr: SocketAddr) -> Result<Self, DiscoveryConfigError> {
        let bind_value = env::var(DISCOVERY_BIND_ADDR_ENV)
            .unwrap_or_else(|_| DEFAULT_DISCOVERY_BIND_ADDR.to_owned());
        let bind_addr = bind_value
            .parse()
            .map_err(|source| DiscoveryConfigError::InvalidBindAddr { source })?;
        let advertise_url = match env::var(DISCOVERY_ADVERTISE_URL_ENV) {
            Ok(value) if value.trim().is_empty() => None,
            Ok(value) => Some(value),
            Err(env::VarError::NotPresent) => None,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(DiscoveryConfigError::InvalidAdvertiseUrl);
            }
        };
        Self::new(bind_addr, http_addr, advertise_url)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum DiscoveryConfigError {
    InvalidBindAddr { source: std::net::AddrParseError },
    InvalidAdvertiseUrl,
}

impl fmt::Display for DiscoveryConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBindAddr { source } => {
                write!(formatter, "invalid {DISCOVERY_BIND_ADDR_ENV}: {source}")
            }
            Self::InvalidAdvertiseUrl => formatter.write_str(
                "invalid LUX_DISCOVERY_ADVERTISE_URL: expected an http or https URL without credentials, query, or fragment",
            ),
        }
    }
}

impl std::error::Error for DiscoveryConfigError {}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DiscoveryResponse {
    #[serde(rename = "Address")]
    pub address: String,
    #[serde(rename = "Id")]
    pub id: String,
    #[serde(rename = "Name")]
    pub name: String,
}

pub struct DiscoveryService {
    socket: UdpSocket,
    config: DiscoveryConfig,
    server_id: String,
    server_name: String,
}

impl DiscoveryService {
    pub async fn bind(
        config: DiscoveryConfig,
        server_id: impl Into<String>,
        server_name: impl Into<String>,
    ) -> io::Result<Self> {
        let socket = UdpSocket::bind(config.bind_addr).await?;
        Ok(Self {
            socket,
            config,
            server_id: sanitize_identifier(&server_id.into()),
            server_name: sanitize_server_name(&server_name.into()),
        })
    }

    pub async fn bind_with_database(
        config: DiscoveryConfig,
        database: &Database,
    ) -> io::Result<Self> {
        let server_name = match database.server_name().await {
            Ok(Some(name)) if !name.trim().is_empty() => name,
            Ok(_) | Err(_) => DEFAULT_SERVER_NAME.to_owned(),
        };
        Self::bind(config, database.server_id().to_owned(), server_name).await
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub async fn run(self, mut shutdown: watch::Receiver<bool>) -> io::Result<()> {
        let mut buffer = [0_u8; MAX_DISCOVERY_DATAGRAM_BYTES];
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            tokio::select! {
                received = self.socket.recv_from(&mut buffer) => {
                    let (length, peer) = received?;
                    if let Err(error) = self.respond(&buffer[..length], peer).await {
                        tracing::warn!(%error, "failed to send LAN discovery response");
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn respond(&self, packet: &[u8], peer: SocketAddr) -> io::Result<()> {
        if peer.port() == 0 || packet.is_empty() || packet.len() > MAX_DISCOVERY_DATAGRAM_BYTES {
            return Ok(());
        }
        let Some(encoding) = request_encoding(packet) else {
            return Ok(());
        };
        let response = DiscoveryResponse {
            address: self.response_address(peer).await,
            id: self.server_id.clone(),
            name: self.server_name.clone(),
        };
        let payload = encode_response(&response, encoding)?;
        self.socket.send_to(&payload, peer).await.map(|_| ())
    }

    async fn response_address(&self, peer: SocketAddr) -> String {
        if let Some(advertise_url) = &self.config.advertise_url {
            return advertise_url.clone();
        }

        let configured_ip = self.config.http_addr.ip();
        let ip = if !configured_ip.is_unspecified()
            && (!configured_ip.is_loopback() || peer.ip().is_loopback())
        {
            configured_ip
        } else {
            local_interface_ip(peer).await.unwrap_or(configured_ip)
        };
        format_http_url(ip, self.config.http_addr.port())
    }
}

fn normalize_advertise_url(value: &str) -> Result<String, DiscoveryConfigError> {
    let value = value.trim();
    let url = Url::parse(value).map_err(|_| DiscoveryConfigError::InvalidAdvertiseUrl)?;
    if value.is_empty()
        || value.len() > MAX_ADVERTISE_URL_LENGTH
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(DiscoveryConfigError::InvalidAdvertiseUrl);
    }
    Ok(url.to_string())
}

fn sanitize_identifier(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_SERVER_ID_LENGTH)
        .collect()
}

fn sanitize_server_name(value: &str) -> String {
    let name: String = value
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_SERVER_NAME_LENGTH)
        .collect();
    if name.is_empty() {
        DEFAULT_SERVER_NAME.to_owned()
    } else {
        name
    }
}

fn request_encoding(packet: &[u8]) -> Option<DiscoveryEncoding> {
    if let Ok(text) = str::from_utf8(packet)
        && text.to_ascii_lowercase().contains(DISCOVERY_REQUEST)
    {
        return Some(DiscoveryEncoding::Utf8);
    }
    if packet.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = packet
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    String::from_utf16(&units)
        .ok()
        .filter(|text| text.to_ascii_lowercase().contains(DISCOVERY_REQUEST))
        .map(|_| DiscoveryEncoding::Utf16Le)
}

fn encode_response(
    response: &DiscoveryResponse,
    encoding: DiscoveryEncoding,
) -> io::Result<Vec<u8>> {
    let json = serde_json::to_string(response)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    match encoding {
        DiscoveryEncoding::Utf8 => Ok(json.into_bytes()),
        DiscoveryEncoding::Utf16Le => {
            let mut bytes = Vec::with_capacity(json.len() * 2);
            for code_unit in json.encode_utf16() {
                bytes.extend_from_slice(&code_unit.to_le_bytes());
            }
            Ok(bytes)
        }
    }
}

async fn local_interface_ip(peer: SocketAddr) -> Option<IpAddr> {
    let unspecified = match peer.ip() {
        IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    let socket = UdpSocket::bind(unspecified).await.ok()?;
    socket.connect(peer).await.ok()?;
    socket.local_addr().ok().map(|address| address.ip())
}

fn format_http_url(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(ip) => format!("http://{ip}:{port}"),
        IpAddr::V6(ip) => format!("http://[{ip}]:{port}"),
    }
}

#[derive(Clone, Copy)]
enum DiscoveryEncoding {
    Utf8,
    Utf16Le,
}

#[cfg(test)]
mod tests {
    use super::{DiscoveryConfig, DiscoveryConfigError, format_http_url, request_encoding};
    use std::net::{IpAddr, Ipv6Addr, SocketAddr};

    #[test]
    fn advertised_url_is_normalized_and_restricted_to_http() {
        let config = DiscoveryConfig::new(
            SocketAddr::from(([127, 0, 0, 1], 7359)),
            SocketAddr::from(([127, 0, 0, 1], 8097)),
            Some(" https://media.example.test/lux ".to_owned()),
        )
        .expect("valid advertised URL");
        assert_eq!(
            config.advertise_url.as_deref(),
            Some("https://media.example.test/lux")
        );
        assert!(matches!(
            DiscoveryConfig::new(
                config.bind_addr,
                config.http_addr,
                Some("https://user:pass@media.example.test".to_owned()),
            ),
            Err(DiscoveryConfigError::InvalidAdvertiseUrl)
        ));
    }

    #[test]
    fn request_matching_accepts_utf8_and_utf16le_only() {
        assert!(request_encoding(b"WHO IS EMBYSERVER?").is_some());
        let utf16: Vec<u8> = "who is EmbyServer?"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(request_encoding(&utf16).is_some());
        assert!(request_encoding(b"who is JellyfinServer?").is_none());
        assert!(request_encoding(&[b'w', 0, b'h']).is_none());
    }

    #[test]
    fn ipv6_discovery_address_has_brackets() {
        assert_eq!(
            format_http_url(IpAddr::V6(Ipv6Addr::LOCALHOST), 8097),
            "http://[::1]:8097"
        );
    }
}
