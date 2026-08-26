use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc, time::Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    sync::OnceCell,
    time::{Duration, sleep},
};

use crate::{
    application::provider_cache::{CacheLookup, ProviderResponseCache, cache_key, ttl_for_method},
    application::{
        plugin_protocol::{
            METADATA_BUNDLE_CAPABILITY, METADATA_CREDITS_CAPABILITY,
            METADATA_EXTERNAL_IDS_CAPABILITY, METADATA_GET_CAPABILITY, METADATA_IMAGES_CAPABILITY,
            METADATA_SEARCH_CAPABILITY, METADATA_TRAILERS_CAPABILITY,
        },
        plugins::{PluginService, PluginServiceError},
    },
    library::LibraryScraperRole,
    observability::resources::ResourceMetrics,
    storage::{Database, StorageError},
};

const SCRAPER_MAX_RETRIES: u32 = 2;
const SCRAPER_RETRY_DELAY: Duration = Duration::from_millis(300);

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

pub type ScraperFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Provider-neutral adapter contract used by application services.
///
/// Concrete providers own their endpoint models and implement this contract
/// in their adapter module. The application layer only sees the scraper
/// request/response models defined in this module.
pub trait ScraperAdapter: Send + Sync {
    fn provider_key(&self) -> &str;

    fn plugin_id(&self) -> Option<&str> {
        None
    }

    fn search(
        &self,
        request: ScraperSearchRequest,
    ) -> ScraperFuture<'_, Result<ScraperSearchResponse, ScraperError>>;

    fn get(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperMetadata, ScraperError>>;

    fn bundle(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperMetadataBundle, ScraperError>>;

    fn images(
        &self,
        request: ScraperImageRequest,
    ) -> ScraperFuture<'_, Result<ScraperImagesResponse, ScraperError>>;

    fn credits(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperCreditsResponse, ScraperError>>;

    fn external_ids(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperExternalIdsResponse, ScraperError>>;

    fn trailers(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperTrailersResponse, ScraperError>>;

    fn configure_api_key(&self, _api_key: Option<String>) -> ScraperFuture<'_, ()> {
        Box::pin(std::future::ready(()))
    }

    fn with_resource_metrics(&self, _resources: ResourceMetrics) {}

    fn clear_response_cache(&self) -> ScraperFuture<'_, ()> {
        Box::pin(std::future::ready(()))
    }
}

/// Provider-neutral handle shared by metadata application services.
#[derive(Clone)]
pub enum ScraperProvider {
    Adapter(Arc<dyn ScraperAdapter>),
    Generic(Box<ScraperPluginClient>),
}

impl ScraperProvider {
    pub fn from_adapter<A>(adapter: A) -> Self
    where
        A: ScraperAdapter + 'static,
    {
        Self::Adapter(Arc::new(adapter))
    }

    pub fn from_scraper(client: ScraperPluginClient) -> Self {
        Self::Generic(Box::new(client))
    }

    pub fn plugin_id(&self) -> Option<&str> {
        match self {
            Self::Adapter(adapter) => adapter.plugin_id(),
            Self::Generic(client) => Some(client.plugin_id()),
        }
    }

    pub fn provider_key(&self) -> &str {
        match self {
            Self::Adapter(adapter) => adapter.provider_key(),
            Self::Generic(client) => client.provider_key(),
        }
    }

    pub(crate) fn with_resource_metrics(self, resources: ResourceMetrics) -> Self {
        match self {
            Self::Adapter(adapter) => {
                adapter.with_resource_metrics(resources);
                Self::Adapter(adapter)
            }
            Self::Generic(client) => {
                Self::Generic(Box::new((*client).with_resource_metrics(resources)))
            }
        }
    }

    pub fn selected_provider_entry<'a>(
        &self,
        result: &'a ScraperSearchResult,
    ) -> Option<(&'a str, &'a str)> {
        result.selected_provider_entry(self.provider_key())
    }

    pub async fn search_generic(
        &self,
        request: ScraperSearchRequest,
    ) -> Result<ScraperSearchResponse, ScraperError> {
        match self {
            Self::Adapter(adapter) => adapter.search(request).await,
            Self::Generic(client) => client.search(request).await,
        }
    }

    pub async fn get_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperMetadata, ScraperError> {
        match self {
            Self::Adapter(adapter) => adapter.get(request).await,
            Self::Generic(client) => client.get(request).await,
        }
    }

    pub async fn bundle_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperMetadataBundle, ScraperError> {
        match self {
            Self::Adapter(adapter) => adapter.bundle(request).await,
            Self::Generic(client) => client.bundle(request).await,
        }
    }

    pub async fn images_generic(
        &self,
        request: ScraperImageRequest,
    ) -> Result<ScraperImagesResponse, ScraperError> {
        match self {
            Self::Adapter(adapter) => adapter.images(request).await,
            Self::Generic(client) => client.images(request).await,
        }
    }

    pub async fn credits_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperCreditsResponse, ScraperError> {
        match self {
            Self::Adapter(adapter) => adapter.credits(request).await,
            Self::Generic(client) => client.credits(request).await,
        }
    }

    pub async fn external_ids_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperExternalIdsResponse, ScraperError> {
        match self {
            Self::Adapter(adapter) => adapter.external_ids(request).await,
            Self::Generic(client) => client.external_ids(request).await,
        }
    }

    pub async fn trailers_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperTrailersResponse, ScraperError> {
        match self {
            Self::Adapter(adapter) => adapter.trailers(request).await,
            Self::Generic(client) => client.trailers(request).await,
        }
    }

    pub async fn configure_api_key(&self, api_key: Option<&str>) {
        if let Self::Adapter(adapter) = self {
            adapter.configure_api_key(api_key.map(str::to_owned)).await;
        }
    }

    pub(crate) async fn clear_response_cache(&self) {
        match self {
            Self::Adapter(adapter) => adapter.clear_response_cache().await,
            Self::Generic(client) => client.clear_response_cache().await,
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

    use super::{
        PluginServiceError, ScraperError, ScraperSearchResult, decode_bundle_response,
        metadata_capability_for_method, provider_id_for_key, provider_key_from_plugin_id,
        retryable_scraper_error,
    };
    use crate::application::plugin_runtime::PluginRuntimeError;
    use serde_json::json;

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

    #[test]
    fn derives_legacy_provider_key_from_plugin_id() {
        assert_eq!(provider_key_from_plugin_id("org.lux.imdb"), "imdb");
        assert_eq!(provider_key_from_plugin_id("org.example.douban"), "douban");
        assert_eq!(provider_key_from_plugin_id("tmdb"), "tmdb");
    }

    #[test]
    fn selects_the_id_for_the_active_provider_namespace() {
        let provider_ids = BTreeMap::from([
            ("Tmdb".to_owned(), "123".to_owned()),
            ("Imdb".to_owned(), "tt1234567".to_owned()),
            ("Douban".to_owned(), "douban-456".to_owned()),
        ]);

        assert_eq!(
            provider_id_for_key(&provider_ids, "imdb"),
            Some("tt1234567")
        );
        assert_eq!(
            provider_id_for_key(&provider_ids, "org.example.douban"),
            Some("douban-456")
        );
        assert_eq!(provider_id_for_key(&provider_ids, "tvdb"), None);
    }

    #[test]
    fn decodes_metadata_bundle_with_attached_provider_data() {
        let bundle = decode_bundle_response(json!({
            "metadata": {"Name": "Example", "ProviderIds": {"Tmdb": "7"}},
            "images": {"images": [{"Type": "Primary", "Url": "https://image.example/poster.jpg"}]},
            "credits": {"cast": [{"Id": "9", "Name": "演员甲"}], "crew": []},
            "externalIds": {"providerIds": {"Tmdb": "7", "Imdb": "tt7"}},
            "trailers": {"trailers": []}
        }))
        .expect("valid metadata bundle");

        assert_eq!(bundle.metadata.title.as_deref(), Some("Example"));
        assert_eq!(bundle.images.images.len(), 1);
        assert_eq!(bundle.credits.cast[0].provider_id, "9");
        assert_eq!(bundle.external_ids.provider_ids["Imdb"], "tt7");
    }

    #[test]
    fn scraper_retries_only_plugin_process_failures() {
        assert!(retryable_scraper_error(&ScraperError::Plugin(
            PluginServiceError::Runtime(PluginRuntimeError::Timeout),
        )));
        assert!(retryable_scraper_error(&ScraperError::Plugin(
            PluginServiceError::Runtime(PluginRuntimeError::Exited),
        )));
        assert!(!retryable_scraper_error(&ScraperError::Provider(
            "rate limited".to_owned(),
        )));
        assert!(!retryable_scraper_error(&ScraperError::InvalidResponse(
            "invalid payload".to_owned(),
        )));
    }

    #[test]
    fn metadata_methods_map_to_declared_capabilities() {
        assert_eq!(
            metadata_capability_for_method("metadata.search"),
            Some("metadata.search")
        );
        assert_eq!(
            metadata_capability_for_method("metadata.externalIds"),
            Some("metadata.externalIds")
        );
        assert_eq!(metadata_capability_for_method("chapters.lookup"), None);
    }

    #[test]
    fn unsupported_metadata_capability_has_stable_error_text() {
        let error = ScraperError::UnsupportedCapability("metadata.images".to_owned());
        assert_eq!(
            error.to_string(),
            "scraper capability unavailable: metadata.images"
        );
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

/// Derives the provider namespace used by legacy manifests that predate the
/// explicit `providerKey` metadata field.
pub fn provider_key_from_plugin_id(plugin_id: &str) -> String {
    plugin_id
        .trim()
        .rsplit(['.', ':', '/'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

pub fn provider_id_for_key<'a>(
    provider_ids: &'a BTreeMap<String, String>,
    provider_key: &str,
) -> Option<&'a str> {
    let provider_key = provider_key.trim();
    if provider_key.is_empty() {
        return None;
    }
    let short_provider = provider_key_from_plugin_id(provider_key);
    provider_ids
        .iter()
        .find(|(provider, value)| {
            !value.trim().is_empty()
                && (provider.eq_ignore_ascii_case(provider_key)
                    || provider.eq_ignore_ascii_case(&short_provider))
        })
        .map(|(_, value)| value.as_str())
}

pub(crate) fn metadata_capability_for_method(method: &str) -> Option<&'static str> {
    Some(match method {
        "metadata.search" => METADATA_SEARCH_CAPABILITY,
        "metadata.get" => METADATA_GET_CAPABILITY,
        "metadata.bundle" => METADATA_BUNDLE_CAPABILITY,
        "metadata.images" => METADATA_IMAGES_CAPABILITY,
        "metadata.credits" => METADATA_CREDITS_CAPABILITY,
        "metadata.externalIds" => METADATA_EXTERNAL_IDS_CAPABILITY,
        "metadata.trailers" => METADATA_TRAILERS_CAPABILITY,
        _ => return None,
    })
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

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub struct ScraperMetadataBundle {
    pub metadata: ScraperMetadata,
    #[serde(default)]
    pub images: ScraperImagesResponse,
    #[serde(default)]
    pub credits: ScraperCreditsResponse,
    #[serde(rename = "externalIds", alias = "external_ids", default)]
    pub external_ids: ScraperExternalIdsResponse,
    #[serde(default)]
    pub trailers: ScraperTrailersResponse,
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
    provider_key: String,
    capability_cache: Arc<OnceCell<Option<Vec<String>>>>,
    response_cache: ProviderResponseCache,
    resources: Option<ResourceMetrics>,
}

impl ScraperPluginClient {
    pub(crate) fn new_with_provider_key(
        plugins: PluginService,
        plugin_id: impl Into<String>,
        provider_key: impl Into<String>,
        response_cache: ProviderResponseCache,
    ) -> Self {
        Self::new_with_provider_key_and_capabilities(
            plugins,
            plugin_id,
            provider_key,
            None,
            response_cache,
        )
    }

    pub(crate) fn new_with_provider_key_and_capabilities(
        plugins: PluginService,
        plugin_id: impl Into<String>,
        provider_key: impl Into<String>,
        capabilities: Option<Vec<String>>,
        response_cache: ProviderResponseCache,
    ) -> Self {
        let capability_cache = Arc::new(OnceCell::new());
        if let Some(capabilities) = capabilities {
            let _ = capability_cache.set(Some(capabilities));
        }
        Self {
            plugins,
            plugin_id: plugin_id.into(),
            provider_key: provider_key.into(),
            capability_cache,
            response_cache,
            resources: None,
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn provider_key(&self) -> &str {
        &self.provider_key
    }

    pub(crate) fn with_resource_metrics(mut self, resources: ResourceMetrics) -> Self {
        self.response_cache.with_resource_metrics(resources.clone());
        self.resources = Some(resources);
        self
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

    pub async fn bundle(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperMetadataBundle, ScraperError> {
        let value = self.call("metadata.bundle", request).await?;
        decode_bundle_response(value)
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
        if let Some(capability) = metadata_capability_for_method(method) {
            let capabilities = self
                .capability_cache
                .get_or_init(|| async { self.plugins.scraper_capabilities(&self.plugin_id).await })
                .await;
            if capabilities.as_ref().is_some_and(|capabilities| {
                !capabilities
                    .iter()
                    .any(|declared| declared.eq_ignore_ascii_case(capability))
            }) {
                return Err(ScraperError::UnsupportedCapability(capability.to_owned()));
            }
        }
        let started = Instant::now();
        let Some(cache_key) = cache_key(&self.plugin_id, method, &params) else {
            let result = self.call_scraper_with_retry(method, params).await;
            self.record_metadata_call(method, false, started);
            return result;
        };
        let cache_owner = loop {
            match self.response_cache.begin(&cache_key).await {
                CacheLookup::Hit(value) => {
                    self.record_metadata_call(method, true, started);
                    return Ok(value);
                }
                CacheLookup::Negative => {
                    self.record_metadata_call(method, true, started);
                    return Err(ScraperError::Provider(
                        "cached scraper result was not found".to_owned(),
                    ));
                }
                CacheLookup::Wait(waiter) => waiter.await,
                CacheLookup::Owner(owner) => break owner,
            }
        };
        let result = self.call_scraper_with_retry(method, params).await;
        self.record_metadata_call(method, false, started);
        if let Ok(value) = &result {
            self.response_cache
                .store(&cache_key, value, ttl_for_method(method))
                .await;
        } else if result.as_ref().err().is_some_and(is_negative_scraper_error) {
            self.response_cache
                .store_negative(&cache_key, 10 * 60)
                .await;
        }
        cache_owner.finish();
        result
    }

    async fn call_scraper_with_retry(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, ScraperError> {
        let mut retry_count = 0;
        loop {
            let result = self
                .plugins
                .call_scraper(&self.plugin_id, method, params.clone())
                .await
                .map_err(ScraperError::Plugin);
            match result {
                Err(error)
                    if retry_count < SCRAPER_MAX_RETRIES && retryable_scraper_error(&error) =>
                {
                    retry_count += 1;
                    if let Some(resources) = &self.resources {
                        resources.record_metadata_retry(method);
                    }
                    sleep(SCRAPER_RETRY_DELAY * retry_count).await;
                }
                result => return result,
            }
        }
    }

    fn record_metadata_call(&self, method: &str, cache_hit: bool, started: Instant) {
        let Some(resources) = &self.resources else {
            return;
        };
        resources.record_metadata_request(method, cache_hit);
        resources.record_metadata_stage(method, started.elapsed());
    }
}

fn retryable_scraper_error(error: &ScraperError) -> bool {
    matches!(
        error,
        ScraperError::Plugin(PluginServiceError::Runtime(
            crate::application::plugin_runtime::PluginRuntimeError::Io(_)
                | crate::application::plugin_runtime::PluginRuntimeError::Timeout
                | crate::application::plugin_runtime::PluginRuntimeError::Exited
        ))
    )
}

fn is_negative_scraper_error(error: &ScraperError) -> bool {
    match error {
        ScraperError::Provider(message) => {
            let message = message.to_ascii_lowercase();
            message.contains("not found") || message.contains("404")
        }
        ScraperError::Plugin(PluginServiceError::Runtime(
            crate::application::plugin_runtime::PluginRuntimeError::Plugin { code, .. },
        )) => {
            let code = code.to_ascii_lowercase();
            code == "not_found" || code == "notfound" || code == "404"
        }
        ScraperError::Plugin(_)
        | ScraperError::Storage(_)
        | ScraperError::UnsupportedCapability(_)
        | ScraperError::InvalidResponse(_) => false,
    }
}

#[derive(Clone)]
pub struct ScraperResolver {
    database: Database,
    plugins: PluginService,
    resources: Option<ResourceMetrics>,
}

#[derive(Clone)]
pub struct ResolvedScraper {
    pub scraper_id: String,
    pub role: LibraryScraperRole,
    pub provider: ScraperProvider,
}

impl ScraperResolver {
    pub fn new(database: Database, plugins: PluginService) -> Self {
        Self {
            database,
            plugins,
            resources: None,
        }
    }

    pub(crate) fn with_resource_metrics(mut self, resources: ResourceMetrics) -> Self {
        self.resources = Some(resources);
        self
    }

    pub async fn for_item(
        &self,
        item_id: &str,
    ) -> Result<Option<ScraperPluginClient>, ScraperError> {
        let Some(scraper) = self.for_item_ordered(item_id).await?.into_iter().next() else {
            return Ok(None);
        };
        match scraper.provider {
            ScraperProvider::Generic(client) => Ok(Some(*client)),
            ScraperProvider::Adapter(_) => Ok(None),
        }
    }

    pub async fn for_item_ordered(
        &self,
        item_id: &str,
    ) -> Result<Vec<ResolvedScraper>, ScraperError> {
        let configured = self.database.find_item_scrapers(item_id).await?;
        let mut resolved = Vec::with_capacity(configured.len());
        let mut first_error = None;
        for scraper in configured {
            let role = scraper
                .role
                .parse::<LibraryScraperRole>()
                .map_err(|error| ScraperError::InvalidResponse(error.to_string()))?;
            let client = match self.plugins.scraper_client(&scraper.scraper_id).await {
                Ok(client) => {
                    if let Some(resources) = &self.resources {
                        client.with_resource_metrics(resources.clone())
                    } else {
                        client
                    }
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(ScraperError::Plugin(error));
                    }
                    continue;
                }
            };
            resolved.push(ResolvedScraper {
                scraper_id: scraper.scraper_id,
                role,
                provider: ScraperProvider::from_scraper(client),
            });
        }
        if resolved.is_empty()
            && let Some(scraper_id) = self.database.find_item_scraper_id(item_id).await?
        {
            let scraper_id = scraper_id.trim();
            if !scraper_id.is_empty()
                && let Ok(client) = self.plugins.scraper_client(scraper_id).await
            {
                let client = if let Some(resources) = &self.resources {
                    client.with_resource_metrics(resources.clone())
                } else {
                    client
                };
                resolved.push(ResolvedScraper {
                    scraper_id: scraper_id.to_owned(),
                    role: LibraryScraperRole::Primary,
                    provider: ScraperProvider::from_scraper(client),
                });
            }
        }
        if resolved.is_empty()
            && let Some(error) = first_error
        {
            return Err(error);
        }
        Ok(resolved)
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

pub fn decode_bundle_response(value: Value) -> Result<ScraperMetadataBundle, ScraperError> {
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
    UnsupportedCapability(String),
    Provider(String),
    InvalidResponse(String),
}

impl fmt::Display for ScraperError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plugin(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::UnsupportedCapability(capability) => {
                write!(formatter, "scraper capability unavailable: {capability}")
            }
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
