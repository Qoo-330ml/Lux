use std::{fmt, net::IpAddr};

use sha2::{Digest, Sha256};
use url::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebhookEventType {
    MediaAdded,
    MediaRemoved,
    ScanCompleted,
    ScanFailed,
    MetadataUpdated,
    JobFailed,
}

impl WebhookEventType {
    pub const ALL: [Self; 6] = [
        Self::MediaAdded,
        Self::MediaRemoved,
        Self::ScanCompleted,
        Self::ScanFailed,
        Self::MetadataUpdated,
        Self::JobFailed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MediaAdded => "MEDIA_ADDED",
            Self::MediaRemoved => "MEDIA_REMOVED",
            Self::ScanCompleted => "SCAN_COMPLETED",
            Self::ScanFailed => "SCAN_FAILED",
            Self::MetadataUpdated => "METADATA_UPDATED",
            Self::JobFailed => "JOB_FAILED",
        }
    }

    pub fn from_wire_name(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|event_type| event_type.as_str() == value)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum WebhookUrlError {
    Invalid,
    Scheme,
    Credentials,
    QueryOrFragment,
    MissingHost,
    PrivateNetwork,
    DangerousAddress,
}

impl fmt::Display for WebhookUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Invalid => "webhook URL is invalid",
            Self::Scheme => "webhook URL must use http or https",
            Self::Credentials => "webhook URL must not contain credentials",
            Self::QueryOrFragment => "webhook URL must not contain a query or fragment",
            Self::MissingHost => "webhook URL host is missing",
            Self::PrivateNetwork => "webhook URL targets a private network",
            Self::DangerousAddress => "webhook URL targets a dangerous reserved address",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for WebhookUrlError {}

pub fn validate_webhook_url(
    value: &str,
    allow_private_network: bool,
) -> Result<Url, WebhookUrlError> {
    let url = Url::parse(value.trim()).map_err(|_| WebhookUrlError::Invalid)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(WebhookUrlError::Scheme);
    }
    if url.username() != "" || url.password().is_some() {
        return Err(WebhookUrlError::Credentials);
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(WebhookUrlError::QueryOrFragment);
    }
    let Some(host) = url.host_str() else {
        return Err(WebhookUrlError::MissingHost);
    };
    let normalized_host = host.to_ascii_lowercase();
    if normalized_host == "localhost"
        || normalized_host.ends_with(".localhost")
        || normalized_host.ends_with(".local")
        || normalized_host.ends_with(".internal")
        || normalized_host.ends_with(".home.arpa")
    {
        if !allow_private_network {
            return Err(WebhookUrlError::PrivateNetwork);
        }
    }

    if let Ok(address) = host.parse::<IpAddr>() {
        if is_dangerous_address(address) {
            return Err(WebhookUrlError::DangerousAddress);
        }
        if !allow_private_network && is_private_address(address) {
            return Err(WebhookUrlError::PrivateNetwork);
        }
    }

    Ok(url)
}

pub fn canonical_signature(secret: &str, timestamp: &str, body: &[u8]) -> String {
    let mut message = Vec::with_capacity(timestamp.len() + 1 + body.len());
    message.extend_from_slice(timestamp.as_bytes());
    message.push(b'.');
    message.extend_from_slice(body);
    let digest = hmac_sha256(secret.as_bytes(), &message);
    let mut encoded = String::with_capacity(7 + digest.len() * 2);
    encoded.push_str("sha256=");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut normalized_key = [0_u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        normalized_key[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized_key[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36_u8; BLOCK_SIZE];
    let mut outer_pad = [0x5c_u8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        inner_pad[index] ^= normalized_key[index];
        outer_pad[index] ^= normalized_key[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    let digest = outer.finalize();
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest);
    output
}

fn is_dangerous_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(value) => {
            value.is_loopback()
                || value.is_unspecified()
                || value.is_link_local()
                || value.is_multicast()
                || value.octets()[0] == 0
        }
        IpAddr::V6(value) => {
            value.is_loopback()
                || value.is_unspecified()
                || value.is_multicast()
                || value.is_unicast_link_local()
        }
    }
}

fn is_private_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(value) => {
            value.is_private()
                || value.is_link_local()
                || value.octets()[0] == 100 && (value.octets()[1] & 0b1100_0000) == 0b0100_0000
                || value.octets()[0] == 198 && (value.octets()[1] == 18 || value.octets()[1] == 19)
                || value.octets()[0] == 192 && value.octets()[1] == 0 && value.octets()[2] == 0
        }
        IpAddr::V6(value) => {
            let octets = value.octets();
            value.is_unicast_link_local() || (octets[0] & 0xfe) == 0xfc
        }
    }
}
