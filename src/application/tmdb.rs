use std::{
    env, fmt,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use reqwest::{Client, Url};
use serde::Deserialize;
use tokio::{sync::Mutex, time::sleep};

const DEFAULT_BASE_URL: &str = "https://api.themoviedb.org/";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct TmdbClientConfig {
    pub base_url: String,
    pub read_access_token: Option<String>,
    pub timeout: Duration,
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub retry_jitter: Duration,
    pub requests_per_second: u32,
}

impl Default for TmdbClientConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            read_access_token: None,
            timeout: Duration::from_secs(10),
            max_retries: 3,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(4),
            retry_jitter: Duration::from_millis(100),
            requests_per_second: 35,
        }
    }
}

#[derive(Clone)]
pub struct TmdbClient {
    http: Client,
    base_url: Url,
    read_access_token: String,
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
    retry_jitter: Duration,
    request_interval: Option<Duration>,
    next_request: Arc<Mutex<Instant>>,
}

impl TmdbClient {
    pub fn new(config: TmdbClientConfig) -> Result<Self, TmdbError> {
        let token = config
            .read_access_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or(TmdbError::MissingToken)?
            .to_owned();
        let base_url_text = if config.base_url.ends_with('/') {
            config.base_url.clone()
        } else {
            format!("{}/", config.base_url)
        };
        let base_url = Url::parse(&base_url_text)
            .map_err(|error| TmdbError::InvalidBaseUrl(error.to_string()))?;
        if !matches!(base_url.scheme(), "http" | "https") {
            return Err(TmdbError::InvalidBaseUrl(
                "TMDb base URL must use http or https".to_owned(),
            ));
        }
        let request_interval = (config.requests_per_second > 0).then(|| {
            let nanos = (1_000_000_000_u64 / u64::from(config.requests_per_second)).max(1);
            Duration::from_nanos(nanos)
        });
        let http = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|error| TmdbError::ClientBuild(error.to_string()))?;
        Ok(Self {
            http,
            base_url,
            read_access_token: token,
            max_retries: config.max_retries,
            initial_backoff: config.initial_backoff,
            max_backoff: config.max_backoff,
            retry_jitter: config.retry_jitter,
            request_interval,
            next_request: Arc::new(Mutex::new(Instant::now())),
        })
    }

    pub fn from_env() -> Result<Self, TmdbError> {
        let config = TmdbClientConfig {
            base_url: env::var("LUX_TMDB_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned()),
            read_access_token: env::var("LUX_TMDB_READ_ACCESS_TOKEN").ok(),
            ..TmdbClientConfig::default()
        };
        Self::new(config)
    }

    pub async fn search_movies(
        &self,
        query: &str,
        primary_release_year: Option<i32>,
        language: &str,
    ) -> Result<TmdbMovieSearchResponse, TmdbError> {
        let query = query.trim();
        let language = language.trim();
        if query.is_empty() || language.is_empty() {
            return Err(TmdbError::InvalidRequest(
                "movie query and language are required".to_owned(),
            ));
        }
        let mut params = vec![
            ("query", query.to_owned()),
            ("include_adult", "false".to_owned()),
            ("language", language.to_owned()),
            ("page", "1".to_owned()),
        ];
        if let Some(year) = primary_release_year {
            if !(1800..=2200).contains(&year) {
                return Err(TmdbError::InvalidRequest(
                    "release year is out of range".to_owned(),
                ));
            }
            params.push(("primary_release_year", year.to_string()));
        }
        let response: TmdbMovieSearchResponse =
            self.request_json("3/search/movie", &params).await?;
        validate_search_response(&response)?;
        Ok(response)
    }

    pub async fn search_movies_with_english_fallback(
        &self,
        query: &str,
        primary_release_year: Option<i32>,
    ) -> Result<TmdbMovieSearchResponse, TmdbError> {
        let mut localized = self
            .search_movies(query, primary_release_year, "zh-CN")
            .await?;
        if !localized.results.is_empty() && localized.results.iter().all(localized_fields_complete)
        {
            return Ok(localized);
        }
        let english = self
            .search_movies(query, primary_release_year, "en-US")
            .await?;
        if localized.results.is_empty() {
            return Ok(english);
        }
        for result in &mut localized.results {
            let Some(fallback) = english.results.iter().find(|item| item.id == result.id) else {
                continue;
            };
            fill_if_empty(&mut result.title, &fallback.title);
            fill_if_empty(&mut result.original_title, &fallback.original_title);
            fill_if_empty(&mut result.overview, &fallback.overview);
            fill_if_empty(&mut result.release_date, &fallback.release_date);
            fill_if_empty(&mut result.original_language, &fallback.original_language);
        }
        Ok(localized)
    }

    pub async fn movie_details(
        &self,
        movie_id: i64,
        language: &str,
    ) -> Result<TmdbMovieDetails, TmdbError> {
        if movie_id <= 0 || language.trim().is_empty() {
            return Err(TmdbError::InvalidRequest(
                "movie ID and language are required".to_owned(),
            ));
        }
        let endpoint = format!("3/movie/{movie_id}");
        let params = [("language", language.trim().to_owned())];
        let details: TmdbMovieDetails = self.request_json(&endpoint, &params).await?;
        if details.id <= 0 {
            return Err(TmdbError::InvalidResponse(
                "movie details ID is invalid".to_owned(),
            ));
        }
        Ok(details)
    }

    async fn request_json<T>(
        &self,
        endpoint: &str,
        params: &[(impl AsRef<str>, String)],
    ) -> Result<T, TmdbError>
    where
        T: for<'de> Deserialize<'de>,
    {
        let mut url = self
            .base_url
            .join(endpoint)
            .map_err(|error| TmdbError::InvalidBaseUrl(error.to_string()))?;
        {
            let mut query = url.query_pairs_mut();
            for (key, value) in params {
                query.append_pair(key.as_ref(), value);
            }
        }

        let mut retry_count = 0;
        loop {
            self.wait_for_rate_limit().await;
            let response = self
                .http
                .get(url.clone())
                .bearer_auth(&self.read_access_token)
                .send()
                .await;
            let response = match response {
                Ok(response) => response,
                Err(error) if error.is_timeout() => {
                    if retry_count < self.max_retries {
                        self.wait_before_retry(retry_count, None).await;
                        retry_count += 1;
                        continue;
                    }
                    return Err(TmdbError::Timeout);
                }
                Err(error) => return Err(TmdbError::Transport(error.to_string())),
            };
            let status = response.status();
            let retry_after = retry_after(&response);
            if status.is_success() {
                let bytes = response.bytes().await.map_err(classify_transport_error)?;
                if bytes.len() > MAX_RESPONSE_BYTES {
                    return Err(TmdbError::InvalidResponse(
                        "TMDb response is too large".to_owned(),
                    ));
                }
                return serde_json::from_slice(&bytes)
                    .map_err(|error| TmdbError::InvalidResponse(error.to_string()));
            }
            if status == reqwest::StatusCode::NOT_FOUND {
                return Err(TmdbError::NotFound);
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                if retry_count < self.max_retries {
                    self.wait_before_retry(retry_count, retry_after).await;
                    retry_count += 1;
                    continue;
                }
                return Err(TmdbError::RateLimited);
            }
            if status.is_server_error() && retry_count < self.max_retries {
                self.wait_before_retry(retry_count, retry_after).await;
                retry_count += 1;
                continue;
            }
            return Err(TmdbError::Upstream {
                status: status.as_u16(),
            });
        }
    }

    async fn wait_for_rate_limit(&self) {
        let Some(interval) = self.request_interval else {
            return;
        };
        let mut next_request = self.next_request.lock().await;
        let now = Instant::now();
        if *next_request > now {
            sleep(*next_request - now).await;
        }
        *next_request = Instant::now() + interval;
    }

    async fn wait_before_retry(&self, retry_count: u32, retry_after: Option<Duration>) {
        let factor = 1_u32.checked_shl(retry_count.min(31)).unwrap_or(u32::MAX);
        let backoff = self
            .initial_backoff
            .checked_mul(factor)
            .unwrap_or(self.max_backoff)
            .min(self.max_backoff);
        let jitter = if self.retry_jitter.is_zero() {
            Duration::ZERO
        } else {
            let nanos = self.retry_jitter.as_nanos().min(u128::from(u64::MAX));
            let seed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            Duration::from_nanos((seed % nanos.max(1)) as u64)
        };
        let delay = backoff.max(retry_after.unwrap_or_default()) + jitter;
        sleep(delay).await;
    }
}

fn classify_transport_error(error: reqwest::Error) -> TmdbError {
    if error.is_timeout() {
        TmdbError::Timeout
    } else {
        TmdbError::Transport(error.to_string())
    }
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TmdbMovieSearchResponse {
    pub page: i32,
    pub total_pages: i32,
    pub total_results: i32,
    pub results: Vec<TmdbMovieSummary>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TmdbMovieSummary {
    pub id: i64,
    pub title: Option<String>,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub release_date: Option<String>,
    pub original_language: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TmdbMovieDetails {
    pub id: i64,
    pub title: Option<String>,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub release_date: Option<String>,
    pub original_language: Option<String>,
}

fn validate_search_response(response: &TmdbMovieSearchResponse) -> Result<(), TmdbError> {
    if response.page < 1 || response.total_pages < 0 || response.total_results < 0 {
        return Err(TmdbError::InvalidResponse(
            "TMDb search pagination is invalid".to_owned(),
        ));
    }
    if response.results.iter().any(|result| result.id <= 0) {
        return Err(TmdbError::InvalidResponse(
            "TMDb movie result ID is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn localized_fields_complete(result: &TmdbMovieSummary) -> bool {
    [result.title.as_deref(), result.overview.as_deref()]
        .into_iter()
        .all(|value| value.is_some_and(|value| !value.trim().is_empty()))
}

fn fill_if_empty(target: &mut Option<String>, fallback: &Option<String>) {
    if target
        .as_deref()
        .and_then(|value| non_empty(Some(value)))
        .is_none()
    {
        if let Some(value) = fallback.as_deref().and_then(|value| non_empty(Some(value))) {
            *target = Some(value.to_owned());
        }
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[derive(Debug)]
pub enum TmdbError {
    MissingToken,
    InvalidBaseUrl(String),
    ClientBuild(String),
    InvalidRequest(String),
    Timeout,
    Transport(String),
    NotFound,
    RateLimited,
    Upstream { status: u16 },
    InvalidResponse(String),
}

impl fmt::Display for TmdbError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingToken => {
                formatter.write_str("TMDb API read access token is not configured")
            }
            Self::InvalidBaseUrl(error) => write!(formatter, "invalid TMDb base URL: {error}"),
            Self::ClientBuild(error) => {
                write!(formatter, "TMDb HTTP client could not be built: {error}")
            }
            Self::InvalidRequest(error) => write!(formatter, "invalid TMDb request: {error}"),
            Self::Timeout => formatter.write_str("TMDb request timed out"),
            Self::Transport(error) => write!(formatter, "TMDb transport failed: {error}"),
            Self::NotFound => formatter.write_str("TMDb resource was not found"),
            Self::RateLimited => formatter.write_str("TMDb rate limit was exhausted"),
            Self::Upstream { status } => write!(formatter, "TMDb returned HTTP {status}"),
            Self::InvalidResponse(error) => write!(formatter, "invalid TMDb response: {error}"),
        }
    }
}

impl std::error::Error for TmdbError {}
