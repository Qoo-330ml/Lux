use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};

use luxd::application::scraper::{
    ScraperAdapter, ScraperCreditsResponse, ScraperError, ScraperExternalIdsResponse,
    ScraperFuture, ScraperGetRequest, ScraperImage, ScraperImageRequest, ScraperImagesResponse,
    ScraperItemType, ScraperMetadata, ScraperMetadataBundle, ScraperProvider, ScraperSearchRequest,
    ScraperSearchResponse, ScraperSearchResult, ScraperTrailer, ScraperTrailersResponse,
};
use reqwest::Client;
use serde_json::Value;
use tokio::sync::Semaphore;

#[allow(dead_code)]
#[derive(Clone)]
pub struct TestScraperConfig {
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

impl Default for TestScraperConfig {
    fn default() -> Self {
        Self {
            base_url: "http://127.0.0.1:9".to_owned(),
            proxy_url: None,
            api_key: None,
            read_access_token: None,
            timeout: Duration::from_secs(10),
            max_retries: 0,
            initial_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
            retry_jitter: Duration::ZERO,
            requests_per_second: 0,
        }
    }
}

#[derive(Debug)]
pub struct TestScraperError(reqwest::Error);

impl std::fmt::Display for TestScraperError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "test scraper client failed: {}", self.0)
    }
}

impl std::error::Error for TestScraperError {}

#[derive(Clone)]
pub struct TestScraper {
    client: Client,
    base_url: String,
    cache: Arc<Mutex<HashMap<String, Value>>>,
    upstream: Arc<Semaphore>,
}

impl TestScraper {
    pub fn new(config: TestScraperConfig) -> Result<Self, TestScraperError> {
        let client = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(TestScraperError)?;
        Ok(Self {
            client,
            base_url: config.base_url.trim_end_matches('/').to_owned(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            upstream: Arc::new(Semaphore::new(4)),
        })
    }

    #[allow(dead_code)]
    pub fn provider(self) -> ScraperProvider {
        ScraperProvider::from_adapter(self)
    }

    fn fetch(
        &self,
        path: String,
        query: Vec<(&'static str, String)>,
    ) -> ScraperFuture<'_, Result<Value, ScraperError>> {
        Box::pin(async move {
            let _permit = self.upstream.acquire().await.map_err(|_| {
                ScraperError::Provider("fixture provider is unavailable".to_owned())
            })?;
            let cache_key = format!("{path}|{query:?}");
            if let Ok(cache) = self.cache.lock()
                && let Some(value) = cache.get(&cache_key)
            {
                return Ok(value.clone());
            }
            let response = self
                .client
                .get(format!("{}{}", self.base_url, path))
                .query(&query)
                .send()
                .await
                .map_err(|error| ScraperError::Provider(error.to_string()))?;
            let status = response.status();
            if !status.is_success() {
                return Err(ScraperError::Provider(format!(
                    "fixture provider returned HTTP {}",
                    status.as_u16()
                )));
            }
            let value = response
                .json::<Value>()
                .await
                .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?;
            if let Ok(mut cache) = self.cache.lock() {
                cache.insert(cache_key, value.clone());
            }
            Ok(value)
        })
    }

    async fn fetch_value(
        &self,
        path: String,
        query: Vec<(&'static str, String)>,
    ) -> Result<Value, ScraperError> {
        self.fetch(path, query).await
    }
}

impl ScraperAdapter for TestScraper {
    fn provider_key(&self) -> &str {
        "tmdb"
    }

    fn search(
        &self,
        request: ScraperSearchRequest,
    ) -> ScraperFuture<'_, Result<ScraperSearchResponse, ScraperError>> {
        Box::pin(async move {
            let path = match request.item_type {
                ScraperItemType::Movie => "/3/search/movie",
                ScraperItemType::Series => "/3/search/tv",
                item_type => {
                    return Err(ScraperError::Provider(format!(
                        "fixture scraper does not support search for {}",
                        item_type.as_str()
                    )));
                }
            };
            let mut query = vec![
                ("query", request.name),
                ("include_adult", "false".to_owned()),
                ("language", request.language),
                ("page", "1".to_owned()),
            ];
            if let Some(year) = request.year {
                query.push((
                    if matches!(request.item_type, ScraperItemType::Movie) {
                        "primary_release_year"
                    } else {
                        "first_air_date_year"
                    },
                    year.to_string(),
                ));
            }
            let response = self.fetch_value(path.to_owned(), query).await?;
            let items = response
                .get("results")
                .and_then(Value::as_array)
                .ok_or_else(|| ScraperError::InvalidResponse("results is missing".to_owned()))?
                .iter()
                .filter_map(|value| map_search_result(value, request.item_type))
                .collect();
            Ok(ScraperSearchResponse { items })
        })
    }

    fn get(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperMetadata, ScraperError>> {
        Box::pin(async move {
            let path = get_path(&request)?;
            let query = non_empty_query("language", request.language);
            let response = self.fetch_value(path, query).await?;
            Ok(map_metadata(&response, request.item_type))
        })
    }

    fn bundle(
        &self,
        _request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperMetadataBundle, ScraperError>> {
        Box::pin(std::future::ready(Err(
            ScraperError::UnsupportedCapability("metadata.bundle".to_owned()),
        )))
    }

    fn images(
        &self,
        request: ScraperImageRequest,
    ) -> ScraperFuture<'_, Result<ScraperImagesResponse, ScraperError>> {
        Box::pin(async move {
            let path = image_path(&request)?;
            let query = non_empty_query("language", request.language);
            let response = self.fetch_value(path, query).await?;
            Ok(map_images(&response))
        })
    }

    fn credits(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperCreditsResponse, ScraperError>> {
        Box::pin(async move {
            let path = match request.item_type {
                ScraperItemType::Movie => format!("/3/movie/{}/credits", request.provider_id),
                ScraperItemType::Series => format!("/3/tv/{}/credits", request.provider_id),
                item_type => {
                    return Err(ScraperError::Provider(format!(
                        "fixture scraper does not support credits for {}",
                        item_type.as_str()
                    )));
                }
            };
            let response = self.fetch_value(path, Vec::new()).await?;
            Ok(map_credits(&response))
        })
    }

    fn external_ids(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperExternalIdsResponse, ScraperError>> {
        Box::pin(async move {
            let entity = match request.item_type {
                ScraperItemType::Movie => "movie",
                ScraperItemType::Series => "tv",
                ScraperItemType::Person => "person",
                item_type => {
                    return Err(ScraperError::Provider(format!(
                        "fixture scraper does not support external IDs for {}",
                        item_type.as_str()
                    )));
                }
            };
            let response = self
                .fetch_value(
                    format!("/3/{entity}/{}/external_ids", request.provider_id),
                    Vec::new(),
                )
                .await?;
            let mut provider_ids = BTreeMap::from([("Tmdb".to_owned(), request.provider_id)]);
            copy_string(&response, "imdb_id", "Imdb", &mut provider_ids);
            copy_string(&response, "tvdb_id", "Tvdb", &mut provider_ids);
            copy_string(&response, "wikidata_id", "Wikidata", &mut provider_ids);
            Ok(ScraperExternalIdsResponse { provider_ids })
        })
    }

    fn trailers(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperTrailersResponse, ScraperError>> {
        Box::pin(async move {
            let entity = match request.item_type {
                ScraperItemType::Movie => "movie",
                ScraperItemType::Series => "tv",
                item_type => {
                    return Err(ScraperError::Provider(format!(
                        "fixture scraper does not support trailers for {}",
                        item_type.as_str()
                    )));
                }
            };
            let response = self
                .fetch_value(
                    format!("/3/{entity}/{}/videos", request.provider_id),
                    non_empty_query("language", request.language),
                )
                .await?;
            let trailers = response
                .get("results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(map_trailer)
                .collect();
            Ok(ScraperTrailersResponse { trailers })
        })
    }
}

fn get_path(request: &ScraperGetRequest) -> Result<String, ScraperError> {
    let path = match request.item_type {
        ScraperItemType::Movie => format!("/3/movie/{}", request.provider_id),
        ScraperItemType::Series => format!("/3/tv/{}", request.provider_id),
        ScraperItemType::Season => format!(
            "/3/tv/{}/season/{}",
            request.provider_id,
            request
                .season_number
                .ok_or_else(|| { ScraperError::Provider("seasonNumber is required".to_owned()) })?
        ),
        ScraperItemType::Episode => format!(
            "/3/tv/{}/season/{}/episode/{}",
            request.provider_id,
            request
                .season_number
                .ok_or_else(|| { ScraperError::Provider("seasonNumber is required".to_owned()) })?,
            request.episode_number.ok_or_else(|| {
                ScraperError::Provider("episodeNumber is required".to_owned())
            })?
        ),
        ScraperItemType::Person => format!("/3/person/{}", request.provider_id),
        ScraperItemType::BoxSet => format!("/3/collection/{}", request.provider_id),
    };
    Ok(path)
}

fn image_path(request: &ScraperImageRequest) -> Result<String, ScraperError> {
    match request.item_type {
        ScraperItemType::Movie => Ok(format!("/3/movie/{}/images", request.provider_id)),
        ScraperItemType::Series => Ok(format!("/3/tv/{}/images", request.provider_id)),
        ScraperItemType::Season => Ok(format!(
            "/3/tv/{}/season/{}/images",
            request.provider_id,
            request
                .season_number
                .ok_or_else(|| { ScraperError::Provider("seasonNumber is required".to_owned()) })?
        )),
        ScraperItemType::Episode => Ok(format!(
            "/3/tv/{}/season/{}/episode/{}/images",
            request.provider_id,
            request
                .season_number
                .ok_or_else(|| { ScraperError::Provider("seasonNumber is required".to_owned()) })?,
            request.episode_number.ok_or_else(|| {
                ScraperError::Provider("episodeNumber is required".to_owned())
            })?
        )),
        ScraperItemType::Person => Ok(format!("/3/person/{}/images", request.provider_id)),
        item_type => Err(ScraperError::Provider(format!(
            "fixture scraper does not support images for {}",
            item_type.as_str()
        ))),
    }
}

fn non_empty_query(key: &'static str, value: String) -> Vec<(&'static str, String)> {
    (!value.trim().is_empty())
        .then_some((key, value))
        .into_iter()
        .collect()
}

fn map_search_result(value: &Value, item_type: ScraperItemType) -> Option<ScraperSearchResult> {
    let id = value_id(value, "id")?;
    let (title_key, original_key, date_key) = match item_type {
        ScraperItemType::Movie => ("title", "original_title", "release_date"),
        ScraperItemType::Series => ("name", "original_name", "first_air_date"),
        _ => return None,
    };
    let premiere_date = value_string(value, date_key);
    Some(ScraperSearchResult {
        item_type: Some(item_type.as_str().to_owned()),
        title: value_string(value, title_key),
        original_title: value_string(value, original_key),
        overview: value_string(value, "overview"),
        production_year: premiere_date.as_deref().and_then(parse_year),
        rating: value.get("vote_average").and_then(Value::as_f64),
        premiere_date,
        original_language: value_string(value, "original_language"),
        provider_ids: BTreeMap::from([("Tmdb".to_owned(), id)]),
        provider_name: Some("Tmdb".to_owned()),
        image_url: value_string(value, "poster_path").map(|path| image_url(&path)),
        backdrop_image_url: value_string(value, "backdrop_path").map(|path| image_url(&path)),
    })
}

fn map_metadata(value: &Value, item_type: ScraperItemType) -> ScraperMetadata {
    let id = value_id(value, "id").unwrap_or_default();
    let (title_key, original_key, premiere_key) = match item_type {
        ScraperItemType::Movie => ("title", "original_title", "release_date"),
        ScraperItemType::Series => ("name", "original_name", "first_air_date"),
        ScraperItemType::Season | ScraperItemType::Episode => ("name", "", "air_date"),
        ScraperItemType::Person => ("name", "", ""),
        ScraperItemType::BoxSet => ("name", "", ""),
    };
    let overview = value_string(value, "overview").or_else(|| value_string(value, "biography"));
    let collection = value.get("belongs_to_collection").map(|collection| {
        luxd::application::scraper::ScraperCollectionReference {
            provider_id: value_id(collection, "id"),
            name: value_string(collection, "name"),
        }
    });
    let items = value
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| {
            Some(luxd::application::scraper::ScraperMetadataItem {
                item_type: Some("Movie".to_owned()),
                title: value_string(part, "title"),
                production_year: value_string(part, "release_date")
                    .as_deref()
                    .and_then(parse_year),
                provider_ids: BTreeMap::from([("Tmdb".to_owned(), value_id(part, "id")?)]),
            })
        })
        .collect();
    ScraperMetadata {
        item_type: Some(item_type.as_str().to_owned()),
        title: value_string(value, title_key),
        original_title: (!original_key.is_empty())
            .then(|| value_string(value, original_key))
            .flatten(),
        overview,
        birthday: value_string(value, "birthday"),
        deathday: value_string(value, "deathday"),
        known_for_department: value_string(value, "known_for_department"),
        place_of_birth: value_string(value, "place_of_birth"),
        tagline: value_string(value, "tagline"),
        website: value_string(value, "homepage"),
        production_year: value_string(value, premiere_key)
            .as_deref()
            .and_then(parse_year),
        rating: value.get("vote_average").and_then(Value::as_f64),
        votes: value.get("vote_count").and_then(Value::as_i64),
        runtime: value
            .get("runtime")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        premiere_date: (!premiere_key.is_empty())
            .then(|| value_string(value, premiere_key))
            .flatten(),
        original_language: value_string(value, "original_language"),
        end_date: value_string(value, "last_air_date"),
        status: value_string(value, "status"),
        set_name: collection.as_ref().and_then(|value| value.name.clone()),
        set_id: collection
            .as_ref()
            .and_then(|value| value.provider_id.clone()),
        poster_url: value_string(value, "poster_path").map(|path| image_url(&path)),
        backdrop_url: value_string(value, "backdrop_path").map(|path| image_url(&path)),
        certification: value_string(value, "certification"),
        genres: names(value, "genres"),
        countries: names(value, "production_countries"),
        studios: names(value, "production_companies"),
        provider_ids: BTreeMap::from([("Tmdb".to_owned(), id)]),
        collection,
        items,
    }
}

fn map_images(value: &Value) -> ScraperImagesResponse {
    let mut images = Vec::new();
    append_images(&mut images, value, "posters", "Primary");
    append_images(&mut images, value, "backdrops", "Backdrop");
    append_images(&mut images, value, "stills", "Backdrop");
    append_images(&mut images, value, "logos", "Logo");
    append_images(&mut images, value, "profiles", "Profile");
    ScraperImagesResponse { images }
}

fn append_images(target: &mut Vec<ScraperImage>, value: &Value, key: &str, image_type: &str) {
    let Some(items) = value.get(key).and_then(Value::as_array) else {
        return;
    };
    target.extend(items.iter().filter_map(|item| {
        let path = value_string(item, "file_path")?;
        let url = image_url(&path);
        Some(ScraperImage {
            image_type: image_type.to_owned(),
            url: url.clone(),
            thumbnail_url: Some(url),
            language: value_string(item, "iso_639_1"),
            width: item
                .get("width")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok()),
            height: item
                .get("height")
                .and_then(Value::as_i64)
                .and_then(|value| i32::try_from(value).ok()),
            provider_name: Some("Tmdb".to_owned()),
        })
    }));
}

fn map_credits(value: &Value) -> ScraperCreditsResponse {
    let cast = value
        .get("cast")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|credit| {
            Some(luxd::application::scraper::ScraperActorCredit {
                provider_id: value_id(credit, "id")?,
                name: value_string(credit, "name"),
                character: value_string(credit, "character"),
                order: credit
                    .get("order")
                    .and_then(Value::as_i64)
                    .and_then(|value| i32::try_from(value).ok()),
                profile_url: value_string(credit, "profile_path").map(|path| image_url(&path)),
            })
        })
        .collect();
    let crew = value
        .get("crew")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|credit| {
            Some(luxd::application::scraper::ScraperCrewCredit {
                provider_id: value_id(credit, "id")?,
                name: value_string(credit, "name"),
                job: value_string(credit, "job"),
                department: value_string(credit, "department"),
            })
        })
        .collect();
    ScraperCreditsResponse { cast, crew }
}

fn map_trailer(value: &Value) -> Option<ScraperTrailer> {
    let key = value_string(value, "key")?;
    let url = match value_string(value, "site")?.as_str() {
        "YouTube" => format!("https://www.youtube.com/watch?v={key}"),
        "Vimeo" => format!("https://vimeo.com/{key}"),
        _ => return None,
    };
    Some(ScraperTrailer {
        name: value_string(value, "name"),
        url: Some(url),
        video_type: value_string(value, "type"),
        official: value.get("official").and_then(Value::as_bool),
        published_at: value_string(value, "published_at"),
    })
}

fn copy_string(value: &Value, source: &str, target: &str, ids: &mut BTreeMap<String, String>) {
    if let Some(value) = value_string(value, source) {
        ids.insert(target.to_owned(), value);
    }
}

fn names(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| {
            value_string(value, "name").or_else(|| value.as_str().map(str::to_owned))
        })
        .collect()
}

fn value_id(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|value| match value {
        Value::String(value) if !value.trim().is_empty() => Some(value.trim().to_owned()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    })
}

fn value_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn parse_year(value: &str) -> Option<i32> {
    value.get(..4)?.parse().ok()
}

fn image_url(path: &str) -> String {
    format!("https://image.tmdb.org/t/p/w780{path}")
}
