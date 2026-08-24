use std::{fmt, net::IpAddr, time::Duration};

use reqwest::{
    Client, ClientBuilder, Proxy, Url,
    header::{LOCATION, RANGE},
};

use crate::network::{NetworkProxyError, normalize_proxy_url};

const MAX_REDIRECTS: usize = 8;
const MAX_URL_CHARS: usize = 8 * 1024;

#[derive(Debug)]
pub enum StrmPlaybackError {
    InvalidUrl,
    InvalidRedirect,
    MissingRedirectLocation,
    TooManyRedirects,
    UnsupportedStatus(u16),
    RequestFailed,
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
            Self::ProxyConfiguration(error) => write!(formatter, "{error}"),
            Self::ClientBuild(error) => write!(formatter, "STRM playback client failed: {error}"),
        }
    }
}

impl std::error::Error for StrmPlaybackError {}

#[derive(Clone)]
pub struct StrmPlaybackResolver {
    client: Client,
}

impl StrmPlaybackResolver {
    pub fn new() -> Result<Self, StrmPlaybackError> {
        Self::from_builder(Client::builder().no_proxy())
    }

    #[doc(hidden)]
    pub fn new_with_proxy_for_tests(proxy_url: String) -> Result<Self, StrmPlaybackError> {
        let proxy_url =
            normalize_proxy_url(&proxy_url).map_err(StrmPlaybackError::ProxyConfiguration)?;
        let proxy = Proxy::all(proxy_url)
            .map_err(|_| StrmPlaybackError::ProxyConfiguration(NetworkProxyError::InvalidUrl))?;
        Self::from_builder(Client::builder().proxy(proxy))
    }

    fn from_builder(builder: ClientBuilder) -> Result<Self, StrmPlaybackError> {
        let client = builder
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| StrmPlaybackError::ClientBuild(error.to_string()))?;
        Ok(Self { client })
    }

    pub async fn resolve(&self, target: &str) -> Result<Url, StrmPlaybackError> {
        let mut current = validate_url(target)?;
        for _ in 0..=MAX_REDIRECTS {
            let response = self
                .client
                .get(current.clone())
                .header(RANGE, "bytes=0-0")
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
            if status == reqwest::StatusCode::OK || status == reqwest::StatusCode::PARTIAL_CONTENT {
                return Ok(current);
            }
            return Err(StrmPlaybackError::UnsupportedStatus(status.as_u16()));
        }
        Err(StrmPlaybackError::TooManyRedirects)
    }
}

fn validate_url(value: &str) -> Result<Url, StrmPlaybackError> {
    if value.chars().count() > MAX_URL_CHARS {
        return Err(StrmPlaybackError::InvalidUrl);
    }
    let url = Url::parse(value).map_err(|_| StrmPlaybackError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(StrmPlaybackError::InvalidUrl);
    }
    let host = url.host_str().ok_or(StrmPlaybackError::InvalidUrl)?;
    if host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".localhost")
        || host.eq_ignore_ascii_case("metadata.google.internal")
        || host.ends_with(".metadata.google.internal")
        || host.parse::<IpAddr>().is_ok_and(is_disallowed_address)
    {
        return Err(StrmPlaybackError::InvalidUrl);
    }
    Ok(url)
}

fn is_disallowed_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_multicast()
                || address.octets()[0] == 0
        }
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unicast_link_local()
                || address.is_unspecified()
                || address.is_multicast()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::validate_url;

    #[test]
    fn accepts_internal_http_targets_without_hardcoded_paths() {
        let url = validate_url("http://192.168.10.50:2083/custom/resolve?id=1")
            .expect("internal HTTP target should be accepted");
        assert_eq!(url.path(), "/custom/resolve");
    }

    #[test]
    fn rejects_credentials_and_non_http_targets() {
        assert!(validate_url("ftp://example.test/video.mkv").is_err());
        assert!(validate_url("http://user:pass@example.test/video.mkv").is_err());
    }

    #[test]
    fn rejects_loopback_and_metadata_targets_but_allows_lan_targets() {
        assert!(validate_url("http://127.0.0.1:8080/video.mkv").is_err());
        assert!(validate_url("http://localhost:8080/video.mkv").is_err());
        assert!(validate_url("http://169.254.169.254/latest/meta-data").is_err());
        assert!(validate_url("http://192.168.10.50:2083/custom/resolve").is_ok());
    }
}
