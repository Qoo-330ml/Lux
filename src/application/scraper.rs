use std::{collections::BTreeMap, fmt};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    application::plugins::{PluginService, PluginServiceError},
    application::provider_cache::{CacheLookup, ProviderResponseCache, cache_key, ttl_for_method},
    storage::{Database, StorageError},
};

/// Stable item types understood by the scraper RPC contract.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum ScraperItemType {
    Movie,
    Series,
    Season,
    Episode,
    Person,
    BoxSet,
}

impl ScraperItemType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Movie => "Movie",
            Self::Series => "Series",
            Self::Season => "Season",
            Self::Episode => "Episode",
            Self::Person => "Person",
            Self::BoxSet => "BoxSet",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScraperSearchRequest {
    pub item_type: ScraperItemType,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i32>,
    pub language: String,
}

impl ScraperSearchRequest {
    pub fn new(
        item_type: ScraperItemType,
        name: impl Into<String>,
        year: Option<i32>,
        language: impl Into<String>,
    ) -> Self {
        Self {
            item_type,
            name: name.into(),
            year,
            language: language.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScraperGetRequest {
    pub item_type: ScraperItemType,
    pub provider_id: String,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_number: Option<i32>,
}

impl ScraperGetRequest {
    pub fn new(
        item_type: ScraperItemType,
        provider_id: impl Into<String>,
        language: impl Into<String>,
    ) -> Self {
        Self {
            item_type,
            provider_id: provider_id.into(),
            language: language.into(),
            season_number: None,
            episode_number: None,
        }
    }

    pub fn for_season(
        provider_id: impl Into<String>,
        season_number: i32,
        language: impl Into<String>,
    ) -> Self {
        Self {
            item_type: ScraperItemType::Season,
            provider_id: provider_id.into(),
            language: language.into(),
            season_number: Some(season_number),
            episode_number: None,
        }
    }

    pub fn for_episode(
        provider_id: impl Into<String>,
        season_number: i32,
        episode_number: i32,
        language: impl Into<String>,
    ) -> Self {
        Self {
            item_type: ScraperItemType::Episode,
            provider_id: provider_id.into(),
            language: language.into(),
            season_number: Some(season_number),
            episode_number: Some(episode_number),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScraperImageRequest {
    pub item_type: ScraperItemType,
    pub provider_id: String,
    pub language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_number: Option<i32>,
}

impl ScraperImageRequest {
    pub fn new(
        item_type: ScraperItemType,
        provider_id: impl Into<String>,
        language: impl Into<String>,
    ) -> Self {
        Self {
            item_type,
            provider_id: provider_id.into(),
            language: language.into(),
            season_number: None,
            episode_number: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ScraperSearchResponse {
    #[serde(default)]
    pub items: Vec<ScraperSearchResult>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ScraperSearchResult {
    #[serde(rename = "Type", alias = "type", default)]
    pub item_type: Option<String>,
    #[serde(rename = "Name", alias = "name", default)]
    pub title: Option<String>,
    #[serde(rename = "OriginalTitle", alias = "originalTitle", default)]
    pub original_title: Option<String>,
    #[serde(rename = "Overview", alias = "overview", default)]
    pub overview: Option<String>,
    #[serde(rename = "ProductionYear", alias = "productionYear", default)]
    pub production_year: Option<i32>,
    #[serde(
        rename = "Rating",
        alias = "rating",
        alias = "VoteAverage",
        alias = "voteAverage",
        default
    )]
    pub rating: Option<f64>,
    #[serde(rename = "PremiereDate", alias = "premiereDate", default)]
    pub premiere_date: Option<String>,
    #[serde(rename = "OriginalLanguage", alias = "originalLanguage", default)]
    pub original_language: Option<String>,
    #[serde(rename = "ProviderIds", alias = "providerIds", default)]
    pub provider_ids: BTreeMap<String, String>,
    #[serde(rename = "SearchProviderName", alias = "searchProviderName", default)]
    pub provider_name: Option<String>,
    #[serde(rename = "ImageUrl", alias = "imageUrl", default)]
    pub image_url: Option<String>,
    #[serde(rename = "BackdropImageUrl", alias = "backdropImageUrl", default)]
    pub backdrop_image_url: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::ScraperSearchResult;

    #[test]
    fn selects_the_id_for_the_configured_scraper() {
        let result = ScraperSearchResult {
            provider_ids: BTreeMap::from([
                ("Imdb".to_owned(), "tt123".to_owned()),
                ("Tvdb".to_owned(), "456".to_owned()),
            ]),
            ..ScraperSearchResult::default()
        };

        assert_eq!(
            result.selected_provider_entry("tvdb"),
            Some(("Tvdb", "456")),
        );
        assert_eq!(
            result.selected_provider_entry("org.example.tvdb"),
            Some(("Tvdb", "456")),
        );
        assert_eq!(result.selected_provider_entry("tmdb"), None);

        let only_other_provider = ScraperSearchResult {
            provider_ids: BTreeMap::from([("Imdb".to_owned(), "tt123".to_owned())]),
            ..ScraperSearchResult::default()
        };
        assert_eq!(only_other_provider.selected_provider_entry("tmdb"), None);
    }
}

impl ScraperSearchResult {
    pub fn selected_provider_entry(&self, selected_provider: &str) -> Option<(&str, &str)> {
        let selected_provider = selected_provider.trim();
        if selected_provider.is_empty() {
            return None;
        }
        let short_provider = selected_provider
            .rsplit(['.', ':', '/'])
            .next()
            .unwrap_or(selected_provider);
        let entry = self
            .provider_ids
            .iter()
            .find(|(provider, _)| provider.eq_ignore_ascii_case(selected_provider));
        let entry = entry.or_else(|| {
            (short_provider != selected_provider)
                .then(|| {
                    self.provider_ids
                        .iter()
                        .find(|(provider, _)| provider.eq_ignore_ascii_case(short_provider))
                })
                .flatten()
        });
        entry.map(|(provider, id)| (provider.as_str(), id.as_str()))
    }

    pub fn provider_id(&self, provider: &str) -> Option<&str> {
        self.provider_ids
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(provider))
            .map(|(_, value)| value.as_str())
    }

    pub fn first_provider_id(&self) -> Option<&str> {
        self.provider_ids.values().next().map(String::as_str)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ScraperMetadata {
    #[serde(rename = "Type", alias = "type", default)]
    pub item_type: Option<String>,
    #[serde(rename = "Name", alias = "name", default)]
    pub title: Option<String>,
    #[serde(rename = "OriginalTitle", alias = "originalTitle", default)]
    pub original_title: Option<String>,
    #[serde(rename = "Overview", alias = "overview", default)]
    pub overview: Option<String>,
    #[serde(rename = "Birthday", alias = "birthday", default)]
    pub birthday: Option<String>,
    #[serde(rename = "Deathday", alias = "deathday", default)]
    pub deathday: Option<String>,
    #[serde(rename = "KnownForDepartment", alias = "knownForDepartment", default)]
    pub known_for_department: Option<String>,
    #[serde(rename = "PlaceOfBirth", alias = "placeOfBirth", default)]
    pub place_of_birth: Option<String>,
    #[serde(rename = "Tagline", alias = "tagline", default)]
    pub tagline: Option<String>,
    #[serde(
        rename = "Website",
        alias = "website",
        alias = "Homepage",
        alias = "homepage",
        default
    )]
    pub website: Option<String>,
    #[serde(rename = "ProductionYear", alias = "productionYear", default)]
    pub production_year: Option<i32>,
    #[serde(
        rename = "Rating",
        alias = "rating",
        alias = "VoteAverage",
        alias = "voteAverage",
        default
    )]
    pub rating: Option<f64>,
    #[serde(
        rename = "Votes",
        alias = "votes",
        alias = "VoteCount",
        alias = "voteCount",
        default
    )]
    pub votes: Option<i64>,
    #[serde(rename = "Runtime", alias = "runtime", default)]
    pub runtime: Option<i32>,
    #[serde(rename = "PremiereDate", alias = "premiereDate", default)]
    pub premiere_date: Option<String>,
    #[serde(rename = "OriginalLanguage", alias = "originalLanguage", default)]
    pub original_language: Option<String>,
    #[serde(rename = "EndDate", alias = "endDate", default)]
    pub end_date: Option<String>,
    #[serde(rename = "Status", alias = "status", default)]
    pub status: Option<String>,
    #[serde(rename = "SetName", alias = "setName", default)]
    pub set_name: Option<String>,
    #[serde(rename = "SetId", alias = "setId", default)]
    pub set_id: Option<String>,
    #[serde(rename = "PosterUrl", alias = "posterUrl", default)]
    pub poster_url: Option<String>,
    #[serde(rename = "BackdropUrl", alias = "backdropUrl", default)]
    pub backdrop_url: Option<String>,
    #[serde(
        rename = "OfficialRating",
        alias = "officialRating",
        alias = "Certification",
        alias = "certification",
        default
    )]
    pub certification: Option<String>,
    #[serde(rename = "Genres", alias = "genres", default)]
    pub genres: Vec<String>,
    #[serde(rename = "Countries", alias = "countries", default)]
    pub countries: Vec<String>,
    #[serde(rename = "Studios", alias = "studios", default)]
    pub studios: Vec<String>,
    #[serde(rename = "ProviderIds", alias = "providerIds", default)]
    pub provider_ids: BTreeMap<String, String>,
    #[serde(rename = "BelongsToCollection", alias = "belongsToCollection", default)]
    pub collection: Option<ScraperCollectionReference>,
    #[serde(rename = "Items", alias = "items", default)]
    pub items: Vec<ScraperMetadataItem>,
}

impl ScraperMetadata {
    pub fn provider_id(&self, provider: &str) -> Option<&str> {
        self.provider_ids
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(provider))
            .map(|(_, value)| value.as_str())
    }

    pub fn first_provider_id(&self) -> Option<&str> {
        self.provider_ids.values().next().map(String::as_str)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ScraperCollectionReference {
    #[serde(rename = "Id", alias = "id", default)]
    pub provider_id: Option<String>,
    #[serde(rename = "Name", alias = "name", default)]
    pub name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ScraperMetadataItem {
    #[serde(rename = "Type", alias = "type", default)]
    pub item_type: Option<String>,
    #[serde(rename = "Name", alias = "name", default)]
    pub title: Option<String>,
    #[serde(rename = "ProductionYear", alias = "productionYear", default)]
    pub production_year: Option<i32>,
    #[serde(rename = "ProviderIds", alias = "providerIds", default)]
    pub provider_ids: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ScraperImagesResponse {
    #[serde(default)]
    pub images: Vec<ScraperImage>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ScraperImage {
    #[serde(rename = "Type", alias = "type", default)]
    pub image_type: String,
    #[serde(rename = "Url", alias = "url")]
    pub url: String,
    #[serde(rename = "ThumbnailUrl", alias = "thumbnailUrl", default)]
    pub thumbnail_url: Option<String>,
    #[serde(rename = "Language", alias = "language", default)]
    pub language: Option<String>,
    #[serde(rename = "Width", alias = "width", default)]
    pub width: Option<i32>,
    #[serde(rename = "Height", alias = "height", default)]
    pub height: Option<i32>,
    #[serde(rename = "ProviderName", alias = "providerName", default)]
    pub provider_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ScraperCreditsResponse {
    #[serde(default)]
    pub cast: Vec<ScraperActorCredit>,
    #[serde(default)]
    pub crew: Vec<ScraperCrewCredit>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ScraperActorCredit {
    #[serde(rename = "Id", alias = "id")]
    pub provider_id: String,
    #[serde(rename = "Name", alias = "name", default)]
    pub name: Option<String>,
    #[serde(rename = "Character", alias = "character", default)]
    pub character: Option<String>,
    #[serde(rename = "Order", alias = "order", default)]
    pub order: Option<i32>,
    #[serde(rename = "ProfileUrl", alias = "profileUrl", default)]
    pub profile_url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ScraperCrewCredit {
    #[serde(rename = "Id", alias = "id", default)]
    pub provider_id: String,
    #[serde(rename = "Name", alias = "name", default)]
    pub name: Option<String>,
    #[serde(rename = "Job", alias = "job", default)]
    pub job: Option<String>,
    #[serde(rename = "Department", alias = "department", default)]
    pub department: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ScraperExternalIdsResponse {
    #[serde(rename = "ProviderIds", alias = "providerIds", default)]
    pub provider_ids: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ScraperTrailersResponse {
    #[serde(default)]
    pub trailers: Vec<ScraperTrailer>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
pub struct ScraperTrailer {
    #[serde(rename = "Name", alias = "name", default)]
    pub name: Option<String>,
    #[serde(rename = "Url", alias = "url", default)]
    pub url: Option<String>,
    #[serde(rename = "Type", alias = "type", default)]
    pub video_type: Option<String>,
    #[serde(rename = "Official", alias = "official", default)]
    pub official: Option<bool>,
    #[serde(rename = "PublishedAt", alias = "publishedAt", default)]
    pub published_at: Option<String>,
}

#[derive(Clone)]
pub struct ScraperPluginClient {
    plugins: PluginService,
    plugin_id: String,
    response_cache: ProviderResponseCache,
}

impl ScraperPluginClient {
    pub(crate) fn new(
        plugins: PluginService,
        plugin_id: impl Into<String>,
        response_cache: ProviderResponseCache,
    ) -> Self {
        Self {
            plugins,
            plugin_id: plugin_id.into(),
            response_cache,
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub(crate) async fn clear_response_cache(&self) {
        self.response_cache.clear().await;
    }

    pub async fn search(
        &self,
        request: ScraperSearchRequest,
    ) -> Result<ScraperSearchResponse, ScraperError> {
        let value = self.call("metadata.search", request).await?;
        decode_search_response(value)
    }

    pub async fn get(&self, request: ScraperGetRequest) -> Result<ScraperMetadata, ScraperError> {
        let value = self.call("metadata.get", request).await?;
        decode_metadata_response(value)
    }

    pub async fn images(
        &self,
        request: ScraperImageRequest,
    ) -> Result<ScraperImagesResponse, ScraperError> {
        let value = self.call("metadata.images", request).await?;
        decode_images_response(value)
    }

    pub async fn credits(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperCreditsResponse, ScraperError> {
        let value = self.call("metadata.credits", request).await?;
        decode_credits_response(value)
    }

    pub async fn external_ids(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperExternalIdsResponse, ScraperError> {
        let value = self.call("metadata.externalIds", request).await?;
        serde_json::from_value(value)
            .map_err(|error| ScraperError::InvalidResponse(error.to_string()))
    }

    pub async fn trailers(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperTrailersResponse, ScraperError> {
        let value = self.call("metadata.trailers", request).await?;
        serde_json::from_value(value)
            .map_err(|error| ScraperError::InvalidResponse(error.to_string()))
    }

    async fn call<T: Serialize>(&self, method: &str, params: T) -> Result<Value, ScraperError> {
        let params = serde_json::to_value(params)
            .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?;
        self.call_value(method, params).await
    }

    pub(crate) async fn call_value(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, ScraperError> {
        let Some(cache_key) = cache_key(&self.plugin_id, method, &params) else {
            return self
                .plugins
                .call_scraper(&self.plugin_id, method, params)
                .await
                .map_err(ScraperError::Plugin);
        };
        loop {
            match self.response_cache.begin(&cache_key).await {
                CacheLookup::Hit(value) => return Ok(value),
                CacheLookup::Wait(notify) => notify.notified().await,
                CacheLookup::Owner => break,
            }
        }
        let result = self
            .plugins
            .call_scraper(&self.plugin_id, method, params)
            .await
            .map_err(ScraperError::Plugin);
        if let Ok(value) = &result {
            self.response_cache
                .store(&cache_key, value, ttl_for_method(method))
                .await;
        }
        self.response_cache.finish(&cache_key).await;
        result
    }
}

#[derive(Clone)]
pub struct ScraperResolver {
    database: Database,
    plugins: PluginService,
}

impl ScraperResolver {
    pub fn new(database: Database, plugins: PluginService) -> Self {
        Self { database, plugins }
    }

    pub async fn for_item(
        &self,
        item_id: &str,
    ) -> Result<Option<ScraperPluginClient>, ScraperError> {
        let Some(scraper_id) = self.database.find_item_scraper_id(item_id).await? else {
            return Ok(None);
        };
        let scraper_id = scraper_id.trim();
        if scraper_id.is_empty() {
            return Ok(None);
        }
        self.plugins
            .scraper_client(scraper_id)
            .await
            .map(Some)
            .map_err(ScraperError::Plugin)
    }
}

pub fn decode_search_response(value: Value) -> Result<ScraperSearchResponse, ScraperError> {
    let items = value
        .get("items")
        .cloned()
        .ok_or_else(|| ScraperError::InvalidResponse("scraper response lacks items".to_owned()))?;
    let items = serde_json::from_value(items)
        .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?;
    Ok(ScraperSearchResponse { items })
}

pub fn decode_metadata_response(value: Value) -> Result<ScraperMetadata, ScraperError> {
    decode_wrapped(value, "metadata")
}

pub fn decode_images_response(value: Value) -> Result<ScraperImagesResponse, ScraperError> {
    let images = value
        .get("images")
        .cloned()
        .ok_or_else(|| ScraperError::InvalidResponse("scraper response lacks images".to_owned()))?;
    let images = serde_json::from_value(images)
        .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?;
    Ok(ScraperImagesResponse { images })
}

pub fn decode_credits_response(value: Value) -> Result<ScraperCreditsResponse, ScraperError> {
    serde_json::from_value(value).map_err(|error| ScraperError::InvalidResponse(error.to_string()))
}

fn decode_wrapped<T: serde::de::DeserializeOwned>(
    value: Value,
    key: &str,
) -> Result<T, ScraperError> {
    let payload = value
        .get(key)
        .cloned()
        .ok_or_else(|| ScraperError::InvalidResponse(format!("scraper response lacks {key}")))?;
    serde_json::from_value(payload)
        .map_err(|error| ScraperError::InvalidResponse(error.to_string()))
}

#[derive(Debug)]
pub enum ScraperError {
    Plugin(PluginServiceError),
    Storage(StorageError),
    Provider(String),
    InvalidResponse(String),
}

impl fmt::Display for ScraperError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plugin(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::Provider(error) => formatter.write_str(error),
            Self::InvalidResponse(message) => {
                write!(formatter, "invalid scraper response: {message}")
            }
        }
    }
}

impl std::error::Error for ScraperError {}

impl From<StorageError> for ScraperError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
