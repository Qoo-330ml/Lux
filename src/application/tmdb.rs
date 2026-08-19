use std::{
    env, fmt,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use reqwest::{Client, Url};
use serde::Deserialize;
use tokio::{
    sync::{Mutex, RwLock, Semaphore},
    time::sleep,
};

use crate::application::provider_cache::{
    CacheLookup, ProviderResponseCache, cache_key, tmdb_ttl_for_endpoint,
};
use crate::network::{apply_proxy, proxy_url_from_env};

const DEFAULT_BASE_URL: &str = "https://api.themoviedb.org/";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const TMDB_MAX_CONCURRENT_REQUESTS: usize = 16;
pub const TMDB_REQUESTS_PER_SECOND: u32 = 32;
pub(crate) const EMBEDDED_TMDB_API_KEY: &str = "f6bd687ffa63cd282b6ff2c6877f2669";

#[derive(Clone)]
pub struct TmdbClientConfig {
    pub base_url: String,
    pub proxy_url: Option<String>,
    pub api_key: Option<String>,
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
            proxy_url: None,
            api_key: None,
            read_access_token: None,
            timeout: Duration::from_secs(10),
            max_retries: 3,
            initial_backoff: Duration::from_millis(250),
            max_backoff: Duration::from_secs(4),
            retry_jitter: Duration::from_millis(100),
            requests_per_second: TMDB_REQUESTS_PER_SECOND,
        }
    }
}

#[derive(Clone)]
pub struct TmdbClient {
    http: Client,
    base_url: Url,
    credential: Arc<RwLock<TmdbCredential>>,
    fallback_credential: TmdbCredential,
    max_retries: u32,
    initial_backoff: Duration,
    max_backoff: Duration,
    retry_jitter: Duration,
    request_interval: Option<Duration>,
    next_request: Arc<Mutex<Instant>>,
    request_concurrency: Arc<Semaphore>,
    response_cache: ProviderResponseCache,
}

impl TmdbClient {
    pub fn new(config: TmdbClientConfig) -> Result<Self, TmdbError> {
        Self::new_with_fallback(config, None)
    }

    fn new_with_fallback(
        config: TmdbClientConfig,
        fallback_credential: Option<TmdbCredential>,
    ) -> Result<Self, TmdbError> {
        let credential = credential_from_config(&config).or(fallback_credential);
        let credential = credential.ok_or(TmdbError::MissingToken)?;
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
        let requests_per_second = if config.requests_per_second == 0 {
            TMDB_REQUESTS_PER_SECOND
        } else {
            config.requests_per_second.min(TMDB_REQUESTS_PER_SECOND)
        };
        let request_interval = Some({
            let nanos = (1_000_000_000_u64 / u64::from(requests_per_second)).max(1);
            Duration::from_nanos(nanos)
        });
        let http = Client::builder().timeout(config.timeout);
        let http = apply_proxy(http, config.proxy_url.as_deref())
            .map_err(|error| TmdbError::InvalidProxyUrl(error.to_string()))?
            .build()
            .map_err(|error| TmdbError::ClientBuild(error.to_string()))?;
        Ok(Self {
            http,
            base_url,
            credential: Arc::new(RwLock::new(credential.clone())),
            fallback_credential: credential,
            max_retries: config.max_retries,
            initial_backoff: config.initial_backoff,
            max_backoff: config.max_backoff,
            retry_jitter: config.retry_jitter,
            request_interval,
            next_request: Arc::new(Mutex::new(Instant::now())),
            request_concurrency: Arc::new(Semaphore::new(TMDB_MAX_CONCURRENT_REQUESTS)),
            response_cache: ProviderResponseCache::new(None),
        })
    }

    pub fn with_cache_dir(mut self, cache_dir: impl Into<std::path::PathBuf>) -> Self {
        self.response_cache = ProviderResponseCache::new(Some(cache_dir.into()));
        self
    }

    pub fn from_env() -> Result<Self, TmdbError> {
        Self::from_env_or_token(None)
    }

    pub fn from_env_or_token(fallback_token: Option<String>) -> Result<Self, TmdbError> {
        let read_access_token = env::var("LUX_TMDB_READ_ACCESS_TOKEN")
            .ok()
            .filter(|token| !token.trim().is_empty())
            .or(fallback_token);
        let config = TmdbClientConfig {
            base_url: env::var("LUX_TMDB_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned()),
            proxy_url: proxy_url_from_env()
                .map_err(|error| TmdbError::InvalidProxyUrl(error.to_string()))?,
            api_key: None,
            read_access_token,
            ..TmdbClientConfig::default()
        };
        Self::new(config)
    }

    pub fn from_env_or_config(
        configured_api_key: Option<String>,
        configured_token: Option<String>,
    ) -> Result<Self, TmdbError> {
        Self::from_env_or_config_with_base_url(configured_api_key, configured_token, None)
    }

    pub fn from_env_or_config_with_base_url(
        configured_api_key: Option<String>,
        configured_token: Option<String>,
        configured_base_url: Option<String>,
    ) -> Result<Self, TmdbError> {
        let environment_api_key = env::var("LUX_TMDB_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let environment_token = env::var("LUX_TMDB_READ_ACCESS_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let fallback_credential = environment_api_key
            .clone()
            .map(TmdbCredential::ApiKey)
            .or_else(|| {
                environment_token
                    .clone()
                    .map(TmdbCredential::ReadAccessToken)
            })
            .or_else(|| {
                configured_token
                    .clone()
                    .map(TmdbCredential::ReadAccessToken)
            })
            .unwrap_or_else(|| TmdbCredential::ApiKey(EMBEDDED_TMDB_API_KEY.to_owned()));
        let config = TmdbClientConfig {
            base_url: configured_base_url
                .filter(|value| !value.trim().is_empty())
                .or_else(|| env::var("LUX_TMDB_BASE_URL").ok())
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_owned()),
            proxy_url: proxy_url_from_env()
                .map_err(|error| TmdbError::InvalidProxyUrl(error.to_string()))?,
            api_key: configured_api_key.or(environment_api_key),
            read_access_token: environment_token.or(configured_token),
            ..TmdbClientConfig::default()
        };
        Self::new_with_fallback(config, Some(fallback_credential))
    }

    pub async fn set_api_key(&self, api_key: Option<&str>) {
        let credential = api_key
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| TmdbCredential::ApiKey(value.to_owned()))
            .unwrap_or_else(|| self.fallback_credential.clone());
        *self.credential.write().await = credential;
    }

    pub(crate) async fn clear_response_cache(&self) {
        self.response_cache.clear().await;
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

    pub async fn search_tv(
        &self,
        query: &str,
        first_air_date_year: Option<i32>,
        language: &str,
    ) -> Result<TmdbTvSearchResponse, TmdbError> {
        let query = query.trim();
        let language = language.trim();
        if query.is_empty() || language.is_empty() {
            return Err(TmdbError::InvalidRequest(
                "TV query and language are required".to_owned(),
            ));
        }
        let mut params = vec![
            ("query", query.to_owned()),
            ("include_adult", "false".to_owned()),
            ("language", language.to_owned()),
            ("page", "1".to_owned()),
        ];
        if let Some(year) = first_air_date_year {
            if !(1800..=2200).contains(&year) {
                return Err(TmdbError::InvalidRequest(
                    "first air date year is out of range".to_owned(),
                ));
            }
            params.push(("first_air_date_year", year.to_string()));
        }
        let response: TmdbTvSearchResponse = self.request_json("3/search/tv", &params).await?;
        validate_tv_search_response(&response)?;
        Ok(response)
    }

    pub async fn search_tv_with_english_fallback(
        &self,
        query: &str,
        first_air_date_year: Option<i32>,
    ) -> Result<TmdbTvSearchResponse, TmdbError> {
        let mut localized = self.search_tv(query, first_air_date_year, "zh-CN").await?;
        if !localized.results.is_empty()
            && localized.results.iter().all(localized_tv_fields_complete)
        {
            return Ok(localized);
        }
        let english = self.search_tv(query, first_air_date_year, "en-US").await?;
        if localized.results.is_empty() {
            return Ok(english);
        }
        for result in &mut localized.results {
            let Some(fallback) = english.results.iter().find(|item| item.id == result.id) else {
                continue;
            };
            fill_if_empty(&mut result.name, &fallback.name);
            fill_if_empty(&mut result.original_name, &fallback.original_name);
            fill_if_empty(&mut result.overview, &fallback.overview);
            fill_if_empty(&mut result.first_air_date, &fallback.first_air_date);
            fill_if_empty(&mut result.original_language, &fallback.original_language);
            fill_if_empty(&mut result.poster_path, &fallback.poster_path);
            fill_if_empty(&mut result.backdrop_path, &fallback.backdrop_path);
        }
        Ok(localized)
    }

    pub async fn search_people(
        &self,
        query: &str,
        language: &str,
    ) -> Result<TmdbPersonSearchResponse, TmdbError> {
        let query = query.trim();
        let language = language.trim();
        if query.is_empty() || language.is_empty() {
            return Err(TmdbError::InvalidRequest(
                "person query and language are required".to_owned(),
            ));
        }
        let params = [
            ("query", query.to_owned()),
            ("include_adult", "false".to_owned()),
            ("language", language.to_owned()),
            ("page", "1".to_owned()),
        ];
        let response: TmdbPersonSearchResponse =
            self.request_json("3/search/person", &params).await?;
        validate_person_search_response(&response)?;
        Ok(response)
    }

    pub async fn search_collections(
        &self,
        query: &str,
        language: &str,
    ) -> Result<TmdbCollectionSearchResponse, TmdbError> {
        let query = query.trim();
        let language = language.trim();
        if query.is_empty() || language.is_empty() {
            return Err(TmdbError::InvalidRequest(
                "collection query and language are required".to_owned(),
            ));
        }
        let params = [
            ("query", query.to_owned()),
            ("language", language.to_owned()),
            ("page", "1".to_owned()),
        ];
        let response: TmdbCollectionSearchResponse =
            self.request_json("3/search/collection", &params).await?;
        validate_collection_search_response(&response)?;
        Ok(response)
    }

    pub async fn series_details(
        &self,
        series_id: i64,
        language: &str,
    ) -> Result<TmdbSeriesDetails, TmdbError> {
        validate_id_language(series_id, language, "series")?;
        let endpoint = format!("3/tv/{series_id}");
        let params = [("language", language.trim().to_owned())];
        let details: TmdbSeriesDetails = self.request_json(&endpoint, &params).await?;
        validate_id(details.id, "series details")?;
        Ok(details)
    }

    pub async fn season_details(
        &self,
        series_id: i64,
        season_number: i32,
        language: &str,
    ) -> Result<TmdbSeasonDetails, TmdbError> {
        validate_id_language(series_id, language, "series")?;
        if !(-1..=1000).contains(&season_number) {
            return Err(TmdbError::InvalidRequest(
                "season number is out of range".to_owned(),
            ));
        }
        let endpoint = format!("3/tv/{series_id}/season/{season_number}");
        let params = [("language", language.trim().to_owned())];
        let details: TmdbSeasonDetails = self.request_json(&endpoint, &params).await?;
        validate_id(details.id, "season details")?;
        if details.episodes.iter().any(|episode| episode.id <= 0) {
            return Err(TmdbError::InvalidResponse(
                "season episode ID is invalid".to_owned(),
            ));
        }
        Ok(details)
    }

    pub async fn episode_details(
        &self,
        series_id: i64,
        season_number: i32,
        episode_number: i32,
        language: &str,
    ) -> Result<TmdbEpisodeDetails, TmdbError> {
        validate_id_language(series_id, language, "series")?;
        if !(-1..=1000).contains(&season_number) || !(0..=10000).contains(&episode_number) {
            return Err(TmdbError::InvalidRequest(
                "episode number is out of range".to_owned(),
            ));
        }
        let endpoint = format!("3/tv/{series_id}/season/{season_number}/episode/{episode_number}");
        let params = [("language", language.trim().to_owned())];
        let details: TmdbEpisodeDetails = self.request_json(&endpoint, &params).await?;
        validate_id(details.id, "episode details")?;
        Ok(details)
    }

    pub async fn person_details(
        &self,
        person_id: i64,
        language: &str,
    ) -> Result<TmdbPersonDetails, TmdbError> {
        validate_id_language(person_id, language, "person")?;
        let endpoint = format!("3/person/{person_id}");
        let params = [("language", language.trim().to_owned())];
        let details: TmdbPersonDetails = self.request_json(&endpoint, &params).await?;
        validate_id(details.id, "person details")?;
        Ok(details)
    }

    pub async fn movie_external_ids(&self, movie_id: i64) -> Result<TmdbExternalIds, TmdbError> {
        self.external_ids("movie", movie_id).await
    }

    pub async fn movie_release_dates(
        &self,
        movie_id: i64,
    ) -> Result<TmdbReleaseDatesResponse, TmdbError> {
        validate_id(movie_id, "movie")?;
        let endpoint = format!("3/movie/{movie_id}/release_dates");
        self.request_json(&endpoint, &[] as &[(String, String)])
            .await
    }

    pub async fn tv_external_ids(&self, series_id: i64) -> Result<TmdbExternalIds, TmdbError> {
        self.external_ids("tv", series_id).await
    }

    pub async fn person_external_ids(&self, person_id: i64) -> Result<TmdbExternalIds, TmdbError> {
        self.external_ids("person", person_id).await
    }

    async fn external_ids(
        &self,
        item_type: &str,
        item_id: i64,
    ) -> Result<TmdbExternalIds, TmdbError> {
        validate_id(item_id, item_type)?;
        let endpoint = format!("3/{item_type}/{item_id}/external_ids");
        let ids: TmdbExternalIds = self
            .request_json(&endpoint, &[] as &[(String, String)])
            .await?;
        Ok(ids)
    }

    pub async fn movie_images(
        &self,
        movie_id: i64,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        self.images("movie", movie_id, language).await
    }

    pub async fn movie_credits(
        &self,
        movie_id: i64,
        language: &str,
    ) -> Result<TmdbCreditsResponse, TmdbError> {
        validate_id_language(movie_id, language, "movie")?;
        let endpoint = format!("3/movie/{movie_id}/credits");
        let params = [("language", language.trim().to_owned())];
        let credits: TmdbCreditsResponse = self.request_json(&endpoint, &params).await?;
        validate_credits_response(&credits)?;
        Ok(credits)
    }

    pub async fn tv_credits(
        &self,
        series_id: i64,
        language: &str,
    ) -> Result<TmdbCreditsResponse, TmdbError> {
        validate_id_language(series_id, language, "series")?;
        let endpoint = format!("3/tv/{series_id}/credits");
        let params = [("language", language.trim().to_owned())];
        let credits: TmdbCreditsResponse = self.request_json(&endpoint, &params).await?;
        validate_credits_response(&credits)?;
        Ok(credits)
    }

    pub async fn tv_images(
        &self,
        series_id: i64,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        self.images("tv", series_id, language).await
    }

    pub async fn season_images(
        &self,
        series_id: i64,
        season_number: i32,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        validate_id_language(series_id, language, "series")?;
        if !(-1..=1000).contains(&season_number) {
            return Err(TmdbError::InvalidRequest(
                "season number is out of range".to_owned(),
            ));
        }
        let endpoint = format!("3/tv/{series_id}/season/{season_number}/images");
        let params = [
            ("language", language.trim().to_owned()),
            (
                "include_image_language",
                format!("{},en,null", language.trim()),
            ),
        ];
        self.request_json(&endpoint, &params).await
    }

    pub async fn episode_images(
        &self,
        series_id: i64,
        season_number: i32,
        episode_number: i32,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        validate_id_language(series_id, language, "series")?;
        if !(-1..=1000).contains(&season_number) || !(0..=10000).contains(&episode_number) {
            return Err(TmdbError::InvalidRequest(
                "episode number is out of range".to_owned(),
            ));
        }
        let endpoint =
            format!("3/tv/{series_id}/season/{season_number}/episode/{episode_number}/images");
        let params = [
            ("language", language.trim().to_owned()),
            (
                "include_image_language",
                format!("{},en,null", language.trim()),
            ),
        ];
        self.request_json(&endpoint, &params).await
    }

    pub async fn person_images(
        &self,
        person_id: i64,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        self.images("person", person_id, language).await
    }

    async fn images(
        &self,
        item_type: &str,
        item_id: i64,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        validate_id_language(item_id, language, item_type)?;
        let endpoint = format!("3/{item_type}/{item_id}/images");
        let params = [
            ("language", language.trim().to_owned()),
            (
                "include_image_language",
                format!("{},en,null", language.trim()),
            ),
        ];
        self.request_json(&endpoint, &params).await
    }

    pub async fn movie_videos(
        &self,
        movie_id: i64,
        language: &str,
    ) -> Result<TmdbVideosResponse, TmdbError> {
        self.videos("movie", movie_id, language).await
    }

    pub async fn tv_videos(
        &self,
        series_id: i64,
        language: &str,
    ) -> Result<TmdbVideosResponse, TmdbError> {
        self.videos("tv", series_id, language).await
    }

    async fn videos(
        &self,
        item_type: &str,
        item_id: i64,
        language: &str,
    ) -> Result<TmdbVideosResponse, TmdbError> {
        validate_id_language(item_id, language, item_type)?;
        let endpoint = format!("3/{item_type}/{item_id}/videos");
        let params = [("language", language.trim().to_owned())];
        self.request_json(&endpoint, &params).await
    }

    pub async fn collection_details(
        &self,
        collection_id: i64,
        language: &str,
    ) -> Result<TmdbCollectionDetails, TmdbError> {
        if collection_id <= 0 || language.trim().is_empty() {
            return Err(TmdbError::InvalidRequest(
                "collection ID and language are required".to_owned(),
            ));
        }
        let endpoint = format!("3/collection/{collection_id}");
        let params = [("language", language.trim().to_owned())];
        let details: TmdbCollectionDetails = self.request_json(&endpoint, &params).await?;
        if details.id <= 0 || details.parts.iter().any(|part| part.id <= 0) {
            return Err(TmdbError::InvalidResponse(
                "TMDb collection details ID is invalid".to_owned(),
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
        let params = params
            .iter()
            .map(|(key, value)| (key.as_ref().to_owned(), value.clone()))
            .collect::<Vec<_>>();
        let value = self.request_value(endpoint, &params).await?;
        serde_json::from_value(value).map_err(|error| TmdbError::InvalidResponse(error.to_string()))
    }

    pub async fn request_value(
        &self,
        endpoint: &str,
        params: &[(String, String)],
    ) -> Result<serde_json::Value, TmdbError> {
        let cache_params = serde_json::json!(params);
        let cache_key = cache_key("tmdb", endpoint, &cache_params);
        if let Some(cache_key) = cache_key.as_deref() {
            loop {
                match self.response_cache.begin(cache_key).await {
                    CacheLookup::Negative => return Err(TmdbError::NotFound),
                    CacheLookup::Hit(value) => return Ok(value),
                    CacheLookup::Wait(notify) => {
                        notify.notified().await;
                    }
                    CacheLookup::Owner => break,
                }
            }
        }
        let result = self.request_value_uncached(endpoint, params).await;
        if let Some(cache_key) = cache_key.as_deref() {
            match &result {
                Ok(value) => {
                    self.response_cache
                        .store(cache_key, value, tmdb_ttl_for_endpoint(endpoint))
                        .await;
                }
                Err(TmdbError::NotFound) => {
                    self.response_cache.store_negative(cache_key, 10 * 60).await;
                }
                Err(_) => {}
            }
            self.response_cache.finish(cache_key).await;
        }
        result
    }

    async fn request_value_uncached(
        &self,
        endpoint: &str,
        params: &[(String, String)],
    ) -> Result<serde_json::Value, TmdbError> {
        let mut url = self
            .base_url
            .join(endpoint)
            .map_err(|error| TmdbError::InvalidBaseUrl(error.to_string()))?;
        {
            let mut query = url.query_pairs_mut();
            for (key, value) in params {
                query.append_pair(key, value);
            }
        }

        let mut retry_count = 0;
        loop {
            let request_permit = self
                .request_concurrency
                .acquire()
                .await
                .map_err(|_| TmdbError::Transport("TMDb request limiter closed".to_owned()))?;
            self.wait_for_rate_limit().await;
            let credential = self.credential.read().await.clone();
            let request = self.http.get(url.clone());
            let response = match credential {
                TmdbCredential::ApiKey(api_key) => {
                    request.query(&[("api_key", api_key.as_str())]).send().await
                }
                TmdbCredential::ReadAccessToken(token) => request.bearer_auth(token).send().await,
            };
            let response = match response {
                Ok(response) => response,
                Err(error) if error.is_timeout() => {
                    if retry_count < self.max_retries {
                        drop(request_permit);
                        self.wait_before_retry(retry_count, None).await;
                        retry_count += 1;
                        continue;
                    }
                    return Err(TmdbError::Timeout);
                }
                Err(_) => return Err(TmdbError::Transport("request failed".to_owned())),
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
                    drop(request_permit);
                    self.wait_before_retry(retry_count, retry_after).await;
                    retry_count += 1;
                    continue;
                }
                return Err(TmdbError::RateLimited);
            }
            if status.is_server_error() && retry_count < self.max_retries {
                drop(request_permit);
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

#[derive(Clone)]
enum TmdbCredential {
    ApiKey(String),
    ReadAccessToken(String),
}

fn credential_from_config(config: &TmdbClientConfig) -> Option<TmdbCredential> {
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| TmdbCredential::ApiKey(value.to_owned()))
        .or_else(|| {
            config
                .read_access_token
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| TmdbCredential::ReadAccessToken(value.to_owned()))
        })
}

fn classify_transport_error(error: reqwest::Error) -> TmdbError {
    if error.is_timeout() {
        TmdbError::Timeout
    } else {
        TmdbError::Transport("response body could not be read".to_owned())
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

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TmdbMovieSearchResponse {
    pub page: i32,
    pub total_pages: i32,
    pub total_results: i32,
    pub results: Vec<TmdbMovieSummary>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TmdbMovieSummary {
    pub id: i64,
    pub title: Option<String>,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub release_date: Option<String>,
    pub original_language: Option<String>,
    pub vote_average: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TmdbCollectionReference {
    pub id: i64,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TmdbCollectionPart {
    pub id: i64,
    pub title: Option<String>,
    pub release_date: Option<String>,
    pub poster_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TmdbCollectionDetails {
    pub id: i64,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    #[serde(default)]
    pub parts: Vec<TmdbCollectionPart>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TmdbMovieDetails {
    pub id: i64,
    pub title: Option<String>,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub homepage: Option<String>,
    pub release_date: Option<String>,
    pub status: Option<String>,
    pub original_language: Option<String>,
    pub vote_average: Option<f64>,
    pub vote_count: Option<i64>,
    pub popularity: Option<f64>,
    pub adult: Option<bool>,
    pub budget: Option<i64>,
    pub revenue: Option<i64>,
    pub runtime: Option<i32>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    #[serde(default)]
    pub genres: Vec<TmdbNamedValue>,
    #[serde(default)]
    pub production_countries: Vec<TmdbProductionCountry>,
    #[serde(default)]
    pub production_companies: Vec<TmdbNamedValue>,
    #[serde(skip)]
    pub certification: Option<String>,
    pub belongs_to_collection: Option<TmdbCollectionReference>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbNamedValue {
    pub id: i64,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbProductionCountry {
    pub iso_3166_1: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbCreditsResponse {
    #[serde(default)]
    pub cast: Vec<TmdbCastMember>,
    #[serde(default)]
    pub crew: Vec<TmdbCrewMember>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TmdbCastMember {
    pub id: i64,
    pub name: Option<String>,
    pub character: Option<String>,
    pub profile_path: Option<String>,
    pub order: Option<i32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TmdbCrewMember {
    pub id: i64,
    pub name: Option<String>,
    pub job: Option<String>,
    pub department: Option<String>,
    pub profile_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TmdbTvSearchResponse {
    pub page: i32,
    pub total_pages: i32,
    pub total_results: i32,
    pub results: Vec<TmdbTvSummary>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TmdbTvSummary {
    pub id: i64,
    pub name: Option<String>,
    pub original_name: Option<String>,
    pub overview: Option<String>,
    pub first_air_date: Option<String>,
    pub original_language: Option<String>,
    pub vote_average: Option<f64>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TmdbPersonSearchResponse {
    pub page: i32,
    pub total_pages: i32,
    pub total_results: i32,
    pub results: Vec<TmdbPersonSummary>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TmdbPersonSummary {
    pub id: i64,
    pub name: Option<String>,
    pub known_for_department: Option<String>,
    pub profile_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TmdbCollectionSearchResponse {
    pub page: i32,
    pub total_pages: i32,
    pub total_results: i32,
    pub results: Vec<TmdbCollectionSearchResult>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct TmdbCollectionSearchResult {
    pub id: i64,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct TmdbSeriesDetails {
    pub id: i64,
    pub name: Option<String>,
    pub original_name: Option<String>,
    pub overview: Option<String>,
    pub first_air_date: Option<String>,
    pub last_air_date: Option<String>,
    pub status: Option<String>,
    pub original_language: Option<String>,
    pub vote_average: Option<f64>,
    pub number_of_seasons: Option<i32>,
    pub number_of_episodes: Option<i32>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    #[serde(default)]
    pub seasons: Vec<TmdbSeasonSummary>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbSeasonSummary {
    pub id: i64,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub season_number: Option<i32>,
    pub episode_count: Option<i32>,
    pub poster_path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbSeasonDetails {
    pub id: i64,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub season_number: Option<i32>,
    pub poster_path: Option<String>,
    #[serde(default)]
    pub episodes: Vec<TmdbEpisodeSummary>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbEpisodeSummary {
    pub id: i64,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub episode_number: Option<i32>,
    pub season_number: Option<i32>,
    pub still_path: Option<String>,
    pub runtime: Option<i32>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbEpisodeDetails {
    pub id: i64,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub air_date: Option<String>,
    pub episode_number: Option<i32>,
    pub season_number: Option<i32>,
    pub still_path: Option<String>,
    pub runtime: Option<i32>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbPersonDetails {
    pub id: i64,
    pub name: Option<String>,
    pub biography: Option<String>,
    pub birthday: Option<String>,
    pub deathday: Option<String>,
    pub known_for_department: Option<String>,
    pub place_of_birth: Option<String>,
    pub profile_path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbExternalIds {
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<i64>,
    pub wikidata_id: Option<String>,
    pub facebook_id: Option<String>,
    pub instagram_id: Option<String>,
    pub twitter_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbReleaseDatesResponse {
    #[serde(default)]
    pub results: Vec<TmdbReleaseDateCountry>,
}

impl TmdbReleaseDatesResponse {
    pub fn certification(&self, preferred_region: &str) -> Option<&str> {
        let preferred_region = preferred_region.trim();
        let preferred = self
            .results
            .iter()
            .find(|country| {
                country
                    .iso_3166_1
                    .as_deref()
                    .is_some_and(|region| region.eq_ignore_ascii_case(preferred_region))
            })
            .and_then(|country| {
                country
                    .release_dates
                    .iter()
                    .find_map(|release| non_empty(release.certification.as_deref()))
            });
        preferred.or_else(|| {
            self.results.iter().find_map(|country| {
                country
                    .release_dates
                    .iter()
                    .find_map(|release| non_empty(release.certification.as_deref()))
            })
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbReleaseDateCountry {
    pub iso_3166_1: Option<String>,
    #[serde(default)]
    pub release_dates: Vec<TmdbReleaseDate>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbReleaseDate {
    pub certification: Option<String>,
    pub release_date: Option<String>,
    #[serde(rename = "type")]
    pub release_type: Option<i32>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct TmdbImagesResponse {
    #[serde(default)]
    pub posters: Vec<TmdbImageReference>,
    #[serde(default)]
    pub backdrops: Vec<TmdbImageReference>,
    #[serde(default)]
    pub stills: Vec<TmdbImageReference>,
    #[serde(default)]
    pub logos: Vec<TmdbImageReference>,
    #[serde(default)]
    pub profiles: Vec<TmdbImageReference>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct TmdbImageReference {
    pub file_path: Option<String>,
    pub iso_639_1: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub vote_average: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbVideosResponse {
    #[serde(default)]
    pub results: Vec<TmdbVideoReference>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct TmdbVideoReference {
    pub id: Option<String>,
    pub key: Option<String>,
    pub name: Option<String>,
    pub site: Option<String>,
    #[serde(rename = "type")]
    pub video_type: Option<String>,
    pub official: Option<bool>,
    pub published_at: Option<String>,
    pub iso_639_1: Option<String>,
}

pub(crate) fn validate_search_response(
    response: &TmdbMovieSearchResponse,
) -> Result<(), TmdbError> {
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

pub(crate) fn validate_tv_search_response(
    response: &TmdbTvSearchResponse,
) -> Result<(), TmdbError> {
    validate_pagination(response.page, response.total_pages, response.total_results)?;
    if response.results.iter().any(|result| result.id <= 0) {
        return Err(TmdbError::InvalidResponse(
            "TMDb TV result ID is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_person_search_response(response: &TmdbPersonSearchResponse) -> Result<(), TmdbError> {
    validate_pagination(response.page, response.total_pages, response.total_results)?;
    if response.results.iter().any(|result| result.id <= 0) {
        return Err(TmdbError::InvalidResponse(
            "TMDb person result ID is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_credits_response(response: &TmdbCreditsResponse) -> Result<(), TmdbError> {
    if response.cast.iter().any(|member| member.id <= 0) {
        return Err(TmdbError::InvalidResponse(
            "TMDb cast member ID is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_collection_search_response(
    response: &TmdbCollectionSearchResponse,
) -> Result<(), TmdbError> {
    validate_pagination(response.page, response.total_pages, response.total_results)?;
    if response.results.iter().any(|result| result.id <= 0) {
        return Err(TmdbError::InvalidResponse(
            "TMDb collection result ID is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_pagination(page: i32, total_pages: i32, total_results: i32) -> Result<(), TmdbError> {
    if page < 1 || total_pages < 0 || total_results < 0 {
        return Err(TmdbError::InvalidResponse(
            "TMDb search pagination is invalid".to_owned(),
        ));
    }
    Ok(())
}

pub(crate) fn validate_id_language(
    id: i64,
    language: &str,
    item_type: &str,
) -> Result<(), TmdbError> {
    validate_id(id, item_type)?;
    if language.trim().is_empty() {
        return Err(TmdbError::InvalidRequest(format!(
            "{item_type} ID and language are required"
        )));
    }
    Ok(())
}

pub(crate) fn validate_id(id: i64, item_type: &str) -> Result<(), TmdbError> {
    if id <= 0 {
        return Err(TmdbError::InvalidResponse(format!(
            "{item_type} ID is invalid"
        )));
    }
    Ok(())
}

pub(crate) fn localized_fields_complete(result: &TmdbMovieSummary) -> bool {
    [result.title.as_deref(), result.overview.as_deref()]
        .into_iter()
        .all(|value| value.is_some_and(|value| !value.trim().is_empty()))
}

pub(crate) fn localized_tv_fields_complete(result: &TmdbTvSummary) -> bool {
    [result.name.as_deref(), result.overview.as_deref()]
        .into_iter()
        .all(|value| value.is_some_and(|value| !value.trim().is_empty()))
}

pub fn fill_if_empty(target: &mut Option<String>, fallback: &Option<String>) {
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
    InvalidProxyUrl(String),
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
            Self::InvalidProxyUrl(error) => write!(formatter, "invalid network proxy URL: {error}"),
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
