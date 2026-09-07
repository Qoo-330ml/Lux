use std::{fmt, net::IpAddr, time::Duration};

use reqwest::{
    Client, ClientBuilder, Proxy, Url,
    header::{ACCEPT_ENCODING, CONTENT_RANGE, CONTENT_TYPE, ETAG, LOCATION, RANGE, USER_AGENT},
};

use crate::network::{NetworkProxyError, normalize_proxy_url};

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

#[derive(Clone)]
pub struct StrmPlaybackResolver {
    client: Client,
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

    pub async fn resolve(
        &self,
        target: &str,
        user_agent: Option<&str>,
    ) -> Result<Url, StrmPlaybackError> {
        let mut current = validate_url(target)?;
        let user_agent = user_agent.unwrap_or(DEFAULT_USER_AGENT);
        for _ in 0..=MAX_REDIRECTS {
            let response = self
                .client
                .get(current.clone())
                .header(RANGE, "bytes=0-0")
                .header(USER_AGENT, user_agent)
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

    pub async fn fetch_range(
        &self,
        target: &str,
        range: &str,
        user_agent: Option<&str>,
    ) -> Result<StrmRangeResponse, StrmPlaybackError> {
        validate_range(range)?;
        let mut current = validate_url(target)?;
        let user_agent = user_agent.unwrap_or(DEFAULT_USER_AGENT);
        for _ in 0..=MAX_REDIRECTS {
            let mut response = self
                .client
                .get(current.clone())
                .header(RANGE, upstream_range(range))
                .header(ACCEPT_ENCODING, "identity")
                .header(USER_AGENT, user_agent)
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
            return Ok(StrmRangeResponse {
                content_range,
                content_length,
                content_type,
                etag,
                body,
            });
        }
        Err(StrmPlaybackError::TooManyRedirects)
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
        && requested_end.map_or(true, |requested_end| end <= requested_end)
        && total > end
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
    use super::{
        MAX_RANGE_BYTES, content_range_matches, upstream_range, validate_range, validate_url,
    };

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
