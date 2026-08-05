use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::application::{
    plugins::{PluginService, TMDB_DYNAMIC_PLUGIN_ID},
    scraper::{
        ScraperCreditsResponse, ScraperError, ScraperGetRequest, ScraperImage, ScraperImageRequest,
        ScraperImagesResponse, ScraperItemType, ScraperMetadata, ScraperPluginClient,
        ScraperSearchRequest, ScraperSearchResponse, ScraperSearchResult,
    },
    tmdb::{
        TmdbClient, TmdbCollectionDetails, TmdbCreditsResponse, TmdbEpisodeDetails, TmdbError,
        TmdbImagesResponse, TmdbMovieDetails, TmdbMovieSearchResponse, TmdbSeasonDetails,
        TmdbSeriesDetails, TmdbTvSearchResponse, fill_if_empty, localized_fields_complete,
        localized_tv_fields_complete, validate_id, validate_id_language, validate_search_response,
        validate_tv_search_response,
    },
};

#[derive(Clone)]
pub struct TmdbPluginClient {
    scraper: ScraperPluginClient,
}

impl TmdbPluginClient {
    pub fn new(plugins: PluginService) -> Self {
        Self {
            scraper: ScraperPluginClient::new(plugins, TMDB_DYNAMIC_PLUGIN_ID),
        }
    }

    pub fn from_scraper(scraper: ScraperPluginClient) -> Self {
        Self { scraper }
    }

    pub async fn search_generic(
        &self,
        request: ScraperSearchRequest,
    ) -> Result<ScraperSearchResponse, ScraperError> {
        self.scraper.search(request).await
    }

    pub async fn get_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperMetadata, ScraperError> {
        self.scraper.get(request).await
    }

    pub async fn images_generic(
        &self,
        request: ScraperImageRequest,
    ) -> Result<ScraperImagesResponse, ScraperError> {
        self.scraper.images(request).await
    }

    pub async fn credits_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperCreditsResponse, ScraperError> {
        self.scraper.credits(request).await
    }

    async fn request<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        params: Vec<(String, String)>,
    ) -> Result<T, TmdbError> {
        let (method, request) = generic_request(endpoint, &params)?;
        let value = self
            .scraper
            .call_value(method, request)
            .await
            .map_err(|error| TmdbError::Transport(error.to_string()))?;
        let value = normalize_generic_response(endpoint, value)?;
        serde_json::from_value(value).map_err(|error| TmdbError::InvalidResponse(error.to_string()))
    }

    async fn search_movies(
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
            ("query".to_owned(), query.to_owned()),
            ("include_adult".to_owned(), "false".to_owned()),
            ("language".to_owned(), language.to_owned()),
            ("page".to_owned(), "1".to_owned()),
        ];
        if let Some(year) = primary_release_year {
            if !(1800..=2200).contains(&year) {
                return Err(TmdbError::InvalidRequest(
                    "release year is out of range".to_owned(),
                ));
            }
            params.push(("primary_release_year".to_owned(), year.to_string()));
        }
        let response = self.request("3/search/movie", params).await?;
        validate_search_response(&response)?;
        Ok(response)
    }

    async fn movie_images(
        &self,
        movie_id: i64,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        self.images("movie", movie_id, language).await
    }

    async fn tv_images(
        &self,
        series_id: i64,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        self.images("tv", series_id, language).await
    }

    async fn images(
        &self,
        item_type: &str,
        item_id: i64,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        validate_id_language(item_id, language, item_type)?;
        self.request(
            &format!("3/{item_type}/{item_id}/images"),
            vec![
                ("language".to_owned(), language.trim().to_owned()),
                (
                    "include_image_language".to_owned(),
                    format!("{},en,null", language.trim()),
                ),
            ],
        )
        .await
    }

    async fn season_images(
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
        self.request(
            &format!("3/tv/{series_id}/season/{season_number}/images"),
            vec![
                ("language".to_owned(), language.trim().to_owned()),
                (
                    "include_image_language".to_owned(),
                    format!("{},en,null", language.trim()),
                ),
            ],
        )
        .await
    }

    async fn episode_images(
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
        self.request(
            &format!("3/tv/{series_id}/season/{season_number}/episode/{episode_number}/images"),
            vec![
                ("language".to_owned(), language.trim().to_owned()),
                (
                    "include_image_language".to_owned(),
                    format!("{},en,null", language.trim()),
                ),
            ],
        )
        .await
    }

    async fn movie_details(
        &self,
        movie_id: i64,
        language: &str,
    ) -> Result<TmdbMovieDetails, TmdbError> {
        validate_id_language(movie_id, language, "movie")?;
        let details: TmdbMovieDetails = self
            .request(
                &format!("3/movie/{movie_id}"),
                vec![("language".to_owned(), language.trim().to_owned())],
            )
            .await?;
        validate_id(details.id, "movie details")?;
        Ok(details)
    }

    async fn movie_credits(
        &self,
        movie_id: i64,
        language: &str,
    ) -> Result<TmdbCreditsResponse, TmdbError> {
        validate_id_language(movie_id, language, "movie")?;
        self.request(
            &format!("3/movie/{movie_id}/credits"),
            vec![("language".to_owned(), language.trim().to_owned())],
        )
        .await
    }

    async fn collection_details(
        &self,
        collection_id: i64,
        language: &str,
    ) -> Result<TmdbCollectionDetails, TmdbError> {
        if collection_id <= 0 || language.trim().is_empty() {
            return Err(TmdbError::InvalidRequest(
                "collection ID and language are required".to_owned(),
            ));
        }
        let details: TmdbCollectionDetails = self
            .request(
                &format!("3/collection/{collection_id}"),
                vec![("language".to_owned(), language.trim().to_owned())],
            )
            .await?;
        if details.id <= 0 || details.parts.iter().any(|part| part.id <= 0) {
            return Err(TmdbError::InvalidResponse(
                "TMDb collection details ID is invalid".to_owned(),
            ));
        }
        Ok(details)
    }

    async fn search_movies_with_english_fallback(
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

    async fn search_tv(
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
            ("query".to_owned(), query.to_owned()),
            ("include_adult".to_owned(), "false".to_owned()),
            ("language".to_owned(), language.to_owned()),
            ("page".to_owned(), "1".to_owned()),
        ];
        if let Some(year) = first_air_date_year {
            if !(1800..=2200).contains(&year) {
                return Err(TmdbError::InvalidRequest(
                    "first air date year is out of range".to_owned(),
                ));
            }
            params.push(("first_air_date_year".to_owned(), year.to_string()));
        }
        let response = self.request("3/search/tv", params).await?;
        validate_tv_search_response(&response)?;
        Ok(response)
    }

    async fn search_tv_with_english_fallback(
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
}

fn generic_request(
    endpoint: &str,
    params: &[(String, String)],
) -> Result<(&'static str, Value), TmdbError> {
    let language = parameter(params, "language").unwrap_or("zh-CN");
    let parts = endpoint.split('/').collect::<Vec<_>>();
    let request = match parts.as_slice() {
        ["3", "search", "movie"] => (
            "metadata.search",
            json!({
                "itemType": "Movie",
                "name": parameter(params, "query").unwrap_or_default(),
                "year": parameter(params, "primary_release_year").and_then(parse_i32),
                "language": language,
            }),
        ),
        ["3", "search", "tv"] => (
            "metadata.search",
            json!({
                "itemType": "Series",
                "name": parameter(params, "query").unwrap_or_default(),
                "year": parameter(params, "first_air_date_year").and_then(parse_i32),
                "language": language,
            }),
        ),
        ["3", "movie", id] => (
            "metadata.get",
            json!({"itemType": "Movie", "providerId": id, "language": language}),
        ),
        ["3", "movie", id, "credits"] => (
            "metadata.credits",
            json!({"itemType": "Movie", "providerId": id, "language": language}),
        ),
        ["3", "tv", id, "credits"] => (
            "metadata.credits",
            json!({"itemType": "Series", "providerId": id, "language": language}),
        ),
        ["3", "movie", id, "images"] => (
            "metadata.images",
            json!({"itemType": "Movie", "providerId": id, "language": language}),
        ),
        ["3", "tv", id, "images"] => (
            "metadata.images",
            json!({"itemType": "Series", "providerId": id, "language": language}),
        ),
        ["3", "tv", id, "season", season, "images"] => (
            "metadata.images",
            json!({
                "itemType": "Season",
                "providerId": id,
                "seasonNumber": parse_i32(season),
                "language": language,
            }),
        ),
        [
            "3",
            "tv",
            id,
            "season",
            season,
            "episode",
            episode,
            "images",
        ] => (
            "metadata.images",
            json!({
                "itemType": "Episode",
                "providerId": id,
                "seasonNumber": parse_i32(season),
                "episodeNumber": parse_i32(episode),
                "language": language,
            }),
        ),
        ["3", "collection", id] => (
            "metadata.get",
            json!({"itemType": "BoxSet", "providerId": id, "language": language}),
        ),
        _ => {
            return Err(TmdbError::InvalidRequest(format!(
                "unsupported generic scraper endpoint: {endpoint}"
            )));
        }
    };
    Ok(request)
}

fn normalize_generic_response(endpoint: &str, value: Value) -> Result<Value, TmdbError> {
    let parts = endpoint.split('/').collect::<Vec<_>>();
    match parts.as_slice() {
        ["3", "search", "movie"] => normalize_search(value, "Movie", "releaseDate"),
        ["3", "search", "tv"] => normalize_search(value, "Series", "firstAirDate"),
        ["3", "movie", _] => normalize_metadata(value, "Movie"),
        ["3", "movie", _, "credits"] => normalize_credits(value),
        ["3", "tv", _, "credits"] => normalize_credits(value),
        ["3", "collection", _] => normalize_collection(value),
        ["3", "movie", _, "images"]
        | ["3", "tv", _, "images"]
        | ["3", "tv", _, "season", _, "images"]
        | ["3", "tv", _, "season", _, "episode", _, "images"] => normalize_images(value),
        _ => Err(TmdbError::InvalidResponse(format!(
            "unsupported generic scraper endpoint: {endpoint}"
        ))),
    }
}

fn normalize_search(value: Value, item_type: &str, _date_field: &str) -> Result<Value, TmdbError> {
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| TmdbError::InvalidResponse("scraper search items are missing".to_owned()))?;
    let results = items
        .iter()
        .map(|item| {
            let provider_id = provider_id(item)?;
            let production_year = item.get("ProductionYear").cloned().unwrap_or(Value::Null);
            let mut result = json!({
                "id": provider_id,
                "title": item.get("Name").cloned().unwrap_or(Value::Null),
                "original_title": item.get("OriginalTitle").cloned().unwrap_or(Value::Null),
                "overview": item.get("Overview").cloned().unwrap_or(Value::Null),
                "release_date": date_from_year(&production_year),
                "original_language": item.get("OriginalLanguage").cloned().unwrap_or(Value::Null),
                "vote_average": item.get("Rating").cloned().unwrap_or(Value::Null),
            });
            if item_type == "Series" {
                result["name"] = result["title"].clone();
                result["original_name"] = result["original_title"].clone();
                result["first_air_date"] = date_from_year(&production_year);
                result["poster_path"] = image_path(item.get("ImageUrl")).unwrap_or(Value::Null);
                result["backdrop_path"] =
                    image_path(item.get("BackdropImageUrl")).unwrap_or(Value::Null);
            }
            Ok(result)
        })
        .collect::<Result<Vec<_>, TmdbError>>()?;
    Ok(json!({"page": 1, "total_pages": 1, "total_results": results.len(), "results": results}))
}

fn normalize_metadata(value: Value, _item_type: &str) -> Result<Value, TmdbError> {
    let metadata = value
        .get("metadata")
        .ok_or_else(|| TmdbError::InvalidResponse("scraper metadata is missing".to_owned()))?;
    let provider_id = provider_id(metadata)?;
    let premiere_date = metadata.get("PremiereDate").cloned().unwrap_or_else(|| {
        date_from_year(
            &metadata
                .get("ProductionYear")
                .cloned()
                .unwrap_or(Value::Null),
        )
    });
    let collection = metadata
        .get("BelongsToCollection")
        .and_then(Value::as_object)
        .map(|collection| {
            json!({
                "id": collection.get("Id").cloned().unwrap_or(Value::Null),
                "name": collection.get("Name").cloned().unwrap_or(Value::Null),
            })
        })
        .unwrap_or(Value::Null);
    Ok(json!({
        "id": provider_id,
        "title": metadata.get("Name").cloned().unwrap_or(Value::Null),
        "original_title": metadata.get("OriginalTitle").cloned().unwrap_or(Value::Null),
        "overview": metadata.get("Overview").cloned().unwrap_or(Value::Null),
        "premiere_date": premiere_date,
        "last_air_date": metadata.get("EndDate").cloned().unwrap_or(Value::Null),
        "status": metadata.get("Status").cloned().unwrap_or(Value::Null),
        "original_language": metadata.get("OriginalLanguage").cloned().unwrap_or(Value::Null),
        "vote_average": metadata.get("Rating").cloned().unwrap_or(Value::Null),
        "belongs_to_collection": collection,
    }))
}

fn normalize_collection(value: Value) -> Result<Value, TmdbError> {
    let metadata = value
        .get("metadata")
        .ok_or_else(|| TmdbError::InvalidResponse("scraper collection is missing".to_owned()))?;
    let collection_id = provider_id(metadata)?;
    let parts = metadata
        .get("Items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|item| {
            Ok(json!({
                "id": provider_id(&item)?,
                "title": item.get("Name").cloned().unwrap_or(Value::Null),
                "release_date": date_from_year(&item.get("ProductionYear").cloned().unwrap_or(Value::Null)),
                "poster_path": image_path(item.get("ImageUrl")),
            }))
        })
        .collect::<Result<Vec<_>, TmdbError>>()?;
    Ok(json!({
        "id": collection_id,
        "name": metadata.get("Name").cloned().unwrap_or(Value::Null),
        "overview": metadata.get("Overview").cloned().unwrap_or(Value::Null),
        "poster_path": image_path(metadata.get("ImageUrl")),
        "backdrop_path": image_path(metadata.get("BackdropImageUrl")),
        "parts": parts,
    }))
}

fn normalize_images(value: Value) -> Result<Value, TmdbError> {
    let images = value
        .get("images")
        .and_then(Value::as_array)
        .ok_or_else(|| TmdbError::InvalidResponse("scraper images are missing".to_owned()))?;
    let mut posters = Vec::new();
    let mut backdrops = Vec::new();
    let mut logos = Vec::new();
    let mut profiles = Vec::new();
    for image in images {
        let Some(path) = image_path(image.get("Url")) else {
            continue;
        };
        let reference = json!({
            "file_path": path,
            "iso_639_1": image.get("Language").cloned().unwrap_or(Value::Null),
            "width": image.get("Width").cloned().unwrap_or(Value::Null),
            "height": image.get("Height").cloned().unwrap_or(Value::Null),
        });
        match image
            .get("Type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "Backdrop" => backdrops.push(reference),
            "Logo" => logos.push(reference),
            "Profile" => profiles.push(reference),
            _ => posters.push(reference),
        }
    }
    Ok(json!({"posters": posters, "backdrops": backdrops, "logos": logos, "profiles": profiles}))
}

fn normalize_credits(value: Value) -> Result<Value, TmdbError> {
    let cast = value
        .get("cast")
        .and_then(Value::as_array)
        .ok_or_else(|| TmdbError::InvalidResponse("scraper credits are missing".to_owned()))?
        .iter()
        .map(|actor| {
            Ok(json!({
                "id": provider_id(actor)?,
                "name": actor.get("Name").cloned().unwrap_or(Value::Null),
                "character": actor.get("Character").cloned().unwrap_or(Value::Null),
                "profile_path": image_path(actor.get("ProfileUrl")),
                "order": actor.get("Order").cloned().unwrap_or(Value::Null),
            }))
        })
        .collect::<Result<Vec<_>, TmdbError>>()?;
    Ok(json!({"cast": cast}))
}

fn parameter<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn parse_i32(value: &str) -> Option<i32> {
    value.parse().ok()
}

fn provider_id(value: &Value) -> Result<i64, TmdbError> {
    let provider = value
        .get("ProviderIds")
        .and_then(Value::as_object)
        .and_then(|ids| {
            ids.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("Tmdb"))
                .map(|(_, value)| value)
        })
        .and_then(|value| {
            value
                .as_str()
                .and_then(|value| value.parse().ok())
                .or_else(|| value.as_i64())
        })
        .or_else(|| {
            value.get("Id").and_then(|value| {
                value
                    .as_str()
                    .and_then(|value| value.parse().ok())
                    .or_else(|| value.as_i64())
            })
        })
        .filter(|id: &i64| *id > 0)
        .ok_or_else(|| TmdbError::InvalidResponse("TMDb provider ID is missing".to_owned()))?;
    Ok(provider)
}

fn date_from_year(value: &Value) -> Value {
    value
        .as_i64()
        .and_then(|year| {
            (1800..=2200)
                .contains(&year)
                .then(|| json!(format!("{year:04}-01-01")))
        })
        .unwrap_or(Value::Null)
}

fn image_path(value: Option<&Value>) -> Option<Value> {
    let url = value?.as_str()?;
    let marker = "/t/p/";
    let start = url.find(marker)? + marker.len();
    let path = url[start..].split_once('/')?.1;
    (!path.is_empty()).then(|| Value::String(format!("/{path}")))
}

/// Provider-neutral application boundary. `TmdbProvider` remains as a source
/// compatibility alias for callers that still use the old name.
#[derive(Clone)]
pub enum ScraperProvider {
    Direct(TmdbClient),
    Plugin(TmdbPluginClient),
    Generic(ScraperPluginClient),
}

pub type TmdbProvider = ScraperProvider;

impl From<TmdbClient> for ScraperProvider {
    fn from(client: TmdbClient) -> Self {
        Self::Direct(client)
    }
}

impl ScraperProvider {
    pub fn from_scraper(client: ScraperPluginClient) -> Self {
        if client.plugin_id() == TMDB_DYNAMIC_PLUGIN_ID {
            Self::Plugin(TmdbPluginClient::from_scraper(client))
        } else {
            Self::Generic(client)
        }
    }

    pub fn selected_provider_entry<'a>(
        &self,
        result: &'a ScraperSearchResult,
    ) -> Option<(&'a str, &'a str)> {
        let provider = match self {
            Self::Direct(_) | Self::Plugin(_) => "tmdb",
            Self::Generic(client) => client.plugin_id(),
        };
        result.selected_provider_entry(provider)
    }
}

impl ScraperProvider {
    pub async fn search_movies_with_english_fallback(
        &self,
        query: &str,
        primary_release_year: Option<i32>,
    ) -> Result<TmdbMovieSearchResponse, TmdbError> {
        match self {
            Self::Direct(client) => {
                client
                    .search_movies_with_english_fallback(query, primary_release_year)
                    .await
            }
            Self::Plugin(client) => {
                client
                    .search_movies_with_english_fallback(query, primary_release_year)
                    .await
            }
            Self::Generic(client) => {
                TmdbPluginClient::from_scraper(client.clone())
                    .search_movies_with_english_fallback(query, primary_release_year)
                    .await
            }
        }
    }

    pub async fn search_tv_with_english_fallback(
        &self,
        query: &str,
        first_air_date_year: Option<i32>,
    ) -> Result<TmdbTvSearchResponse, TmdbError> {
        match self {
            Self::Direct(client) => {
                client
                    .search_tv_with_english_fallback(query, first_air_date_year)
                    .await
            }
            Self::Plugin(client) => {
                client
                    .search_tv_with_english_fallback(query, first_air_date_year)
                    .await
            }
            Self::Generic(client) => {
                TmdbPluginClient::from_scraper(client.clone())
                    .search_tv_with_english_fallback(query, first_air_date_year)
                    .await
            }
        }
    }

    pub async fn movie_details(
        &self,
        movie_id: i64,
        language: &str,
    ) -> Result<TmdbMovieDetails, TmdbError> {
        match self {
            Self::Direct(client) => client.movie_details(movie_id, language).await,
            Self::Plugin(client) => client.movie_details(movie_id, language).await,
            Self::Generic(client) => {
                TmdbPluginClient::from_scraper(client.clone())
                    .movie_details(movie_id, language)
                    .await
            }
        }
    }

    pub async fn collection_details(
        &self,
        collection_id: i64,
        language: &str,
    ) -> Result<TmdbCollectionDetails, TmdbError> {
        match self {
            Self::Direct(client) => client.collection_details(collection_id, language).await,
            Self::Plugin(client) => client.collection_details(collection_id, language).await,
            Self::Generic(client) => {
                TmdbPluginClient::from_scraper(client.clone())
                    .collection_details(collection_id, language)
                    .await
            }
        }
    }

    pub async fn movie_credits(
        &self,
        movie_id: i64,
        language: &str,
    ) -> Result<TmdbCreditsResponse, TmdbError> {
        match self {
            Self::Direct(client) => client.movie_credits(movie_id, language).await,
            Self::Plugin(client) => client.movie_credits(movie_id, language).await,
            Self::Generic(client) => {
                TmdbPluginClient::from_scraper(client.clone())
                    .movie_credits(movie_id, language)
                    .await
            }
        }
    }

    pub async fn movie_images(
        &self,
        movie_id: i64,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        match self {
            Self::Direct(client) => client.movie_images(movie_id, language).await,
            Self::Plugin(client) => client.movie_images(movie_id, language).await,
            Self::Generic(client) => {
                TmdbPluginClient::from_scraper(client.clone())
                    .movie_images(movie_id, language)
                    .await
            }
        }
    }

    pub async fn tv_images(
        &self,
        series_id: i64,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        match self {
            Self::Direct(client) => client.tv_images(series_id, language).await,
            Self::Plugin(client) => client.tv_images(series_id, language).await,
            Self::Generic(client) => {
                TmdbPluginClient::from_scraper(client.clone())
                    .tv_images(series_id, language)
                    .await
            }
        }
    }

    pub async fn season_images(
        &self,
        series_id: i64,
        season_number: i32,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        match self {
            Self::Direct(client) => {
                client
                    .season_images(series_id, season_number, language)
                    .await
            }
            Self::Plugin(client) => {
                client
                    .season_images(series_id, season_number, language)
                    .await
            }
            Self::Generic(client) => {
                TmdbPluginClient::from_scraper(client.clone())
                    .season_images(series_id, season_number, language)
                    .await
            }
        }
    }

    pub async fn episode_images(
        &self,
        series_id: i64,
        season_number: i32,
        episode_number: i32,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        match self {
            Self::Direct(client) => {
                client
                    .episode_images(series_id, season_number, episode_number, language)
                    .await
            }
            Self::Plugin(client) => {
                client
                    .episode_images(series_id, season_number, episode_number, language)
                    .await
            }
            Self::Generic(client) => {
                TmdbPluginClient::from_scraper(client.clone())
                    .episode_images(series_id, season_number, episode_number, language)
                    .await
            }
        }
    }

    pub async fn search_generic(
        &self,
        request: ScraperSearchRequest,
    ) -> Result<ScraperSearchResponse, ScraperError> {
        match self {
            Self::Direct(client) => direct_search_generic(client, request).await,
            Self::Plugin(client) => client.search_generic(request).await,
            Self::Generic(client) => client.search(request).await,
        }
    }

    pub async fn get_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperMetadata, ScraperError> {
        match self {
            Self::Direct(client) => direct_get_generic(client, request).await,
            Self::Plugin(client) => client.get_generic(request).await,
            Self::Generic(client) => client.get(request).await,
        }
    }

    pub async fn images_generic(
        &self,
        request: ScraperImageRequest,
    ) -> Result<ScraperImagesResponse, ScraperError> {
        match self {
            Self::Direct(client) => direct_images_generic(client, request).await,
            Self::Plugin(client) => client.images_generic(request).await,
            Self::Generic(client) => client.images(request).await,
        }
    }

    pub async fn credits_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperCreditsResponse, ScraperError> {
        match self {
            Self::Direct(client) => direct_credits_generic(client, request).await,
            Self::Plugin(client) => client.credits_generic(request).await,
            Self::Generic(client) => client.credits(request).await,
        }
    }

    pub async fn set_api_key(&self, api_key: Option<&str>) {
        if let Self::Direct(client) = self {
            client.set_api_key(api_key).await;
        }
    }
}

async fn direct_search_generic(
    client: &TmdbClient,
    request: ScraperSearchRequest,
) -> Result<ScraperSearchResponse, ScraperError> {
    match request.item_type {
        ScraperItemType::Movie => client
            .search_movies_with_english_fallback(&request.name, request.year)
            .await
            .map(movie_search_generic)
            .map_err(|error| ScraperError::Provider(error.to_string())),
        ScraperItemType::Series => client
            .search_tv_with_english_fallback(&request.name, request.year)
            .await
            .map(series_search_generic)
            .map_err(|error| ScraperError::Provider(error.to_string())),
        item_type => Err(ScraperError::Provider(format!(
            "TMDb direct scraper does not support generic search for {}",
            item_type.as_str()
        ))),
    }
}

async fn direct_get_generic(
    client: &TmdbClient,
    request: ScraperGetRequest,
) -> Result<ScraperMetadata, ScraperError> {
    let id = request
        .provider_id
        .parse::<i64>()
        .map_err(|_| ScraperError::Provider("TMDb provider ID is invalid".to_owned()))?;
    match request.item_type {
        ScraperItemType::Movie => client
            .movie_details(id, &request.language)
            .await
            .map(movie_metadata_generic)
            .map_err(|error| ScraperError::Provider(error.to_string())),
        ScraperItemType::Series => client
            .series_details(id, &request.language)
            .await
            .map(series_metadata_generic)
            .map_err(|error| ScraperError::Provider(error.to_string())),
        ScraperItemType::Season => client
            .season_details(
                id,
                request
                    .season_number
                    .ok_or_else(|| ScraperError::Provider("seasonNumber is required".to_owned()))?,
                &request.language,
            )
            .await
            .map(season_metadata_generic)
            .map_err(|error| ScraperError::Provider(error.to_string())),
        ScraperItemType::Episode => client
            .episode_details(
                id,
                request
                    .season_number
                    .ok_or_else(|| ScraperError::Provider("seasonNumber is required".to_owned()))?,
                request.episode_number.ok_or_else(|| {
                    ScraperError::Provider("episodeNumber is required".to_owned())
                })?,
                &request.language,
            )
            .await
            .map(episode_metadata_generic)
            .map_err(|error| ScraperError::Provider(error.to_string())),
        ScraperItemType::BoxSet => client
            .collection_details(id, &request.language)
            .await
            .map(collection_metadata_generic)
            .map_err(|error| ScraperError::Provider(error.to_string())),
        item_type => Err(ScraperError::Provider(format!(
            "TMDb direct scraper does not support generic metadata for {}",
            item_type.as_str()
        ))),
    }
}

async fn direct_images_generic(
    client: &TmdbClient,
    request: ScraperImageRequest,
) -> Result<ScraperImagesResponse, ScraperError> {
    let id = request
        .provider_id
        .parse::<i64>()
        .map_err(|_| ScraperError::Provider("TMDb provider ID is invalid".to_owned()))?;
    let images = match request.item_type {
        ScraperItemType::Movie => client.movie_images(id, &request.language).await,
        ScraperItemType::Series => client.tv_images(id, &request.language).await,
        ScraperItemType::Season => {
            client
                .season_images(
                    id,
                    request.season_number.ok_or_else(|| {
                        ScraperError::Provider("seasonNumber is required".to_owned())
                    })?,
                    &request.language,
                )
                .await
        }
        ScraperItemType::Episode => {
            client
                .episode_images(
                    id,
                    request.season_number.ok_or_else(|| {
                        ScraperError::Provider("seasonNumber is required".to_owned())
                    })?,
                    request.episode_number.ok_or_else(|| {
                        ScraperError::Provider("episodeNumber is required".to_owned())
                    })?,
                    &request.language,
                )
                .await
        }
        item_type => {
            return Err(ScraperError::Provider(format!(
                "TMDb direct scraper does not support images for {}",
                item_type.as_str()
            )));
        }
    }
    .map_err(|error| ScraperError::Provider(error.to_string()))?;
    Ok(tmdb_images_generic(images))
}

async fn direct_credits_generic(
    client: &TmdbClient,
    request: ScraperGetRequest,
) -> Result<ScraperCreditsResponse, ScraperError> {
    let id = request
        .provider_id
        .parse::<i64>()
        .map_err(|_| ScraperError::Provider("TMDb provider ID is invalid".to_owned()))?;
    let response = match request.item_type {
        ScraperItemType::Movie => client.movie_credits(id, &request.language).await,
        ScraperItemType::Series => client.tv_credits(id, &request.language).await,
        item_type => {
            return Err(ScraperError::Provider(format!(
                "TMDb direct scraper does not support credits for {}",
                item_type.as_str()
            )));
        }
    }
    .map_err(|error| ScraperError::Provider(error.to_string()))?;
    Ok(ScraperCreditsResponse {
        cast: response
            .cast
            .into_iter()
            .map(|actor| crate::application::scraper::ScraperActorCredit {
                provider_id: actor.id.to_string(),
                name: actor.name,
                character: actor.character,
                order: actor.order,
                profile_url: actor.profile_path.map(|path| tmdb_image_url(&path)),
            })
            .collect(),
    })
}

fn movie_search_generic(response: TmdbMovieSearchResponse) -> ScraperSearchResponse {
    ScraperSearchResponse {
        items: response
            .results
            .into_iter()
            .map(|result| ScraperSearchResult {
                item_type: Some("Movie".to_owned()),
                title: result.title,
                original_title: result.original_title,
                overview: result.overview,
                production_year: result.release_date.as_deref().and_then(parse_year),
                premiere_date: result.release_date,
                original_language: result.original_language,
                rating: result.vote_average,
                provider_ids: BTreeMap::from([("Tmdb".to_owned(), result.id.to_string())]),
                provider_name: Some("Tmdb".to_owned()),
                image_url: None,
                backdrop_image_url: None,
            })
            .collect(),
    }
}

fn series_search_generic(response: TmdbTvSearchResponse) -> ScraperSearchResponse {
    ScraperSearchResponse {
        items: response
            .results
            .into_iter()
            .map(|result| ScraperSearchResult {
                item_type: Some("Series".to_owned()),
                title: result.name,
                original_title: result.original_name,
                overview: result.overview,
                production_year: result.first_air_date.as_deref().and_then(parse_year),
                premiere_date: result.first_air_date,
                original_language: result.original_language,
                rating: result.vote_average,
                provider_ids: BTreeMap::from([("Tmdb".to_owned(), result.id.to_string())]),
                provider_name: Some("Tmdb".to_owned()),
                image_url: result.poster_path.map(|path| tmdb_image_url(&path)),
                backdrop_image_url: result.backdrop_path.map(|path| tmdb_image_url(&path)),
            })
            .collect(),
    }
}

fn movie_metadata_generic(details: TmdbMovieDetails) -> ScraperMetadata {
    ScraperMetadata {
        item_type: Some("Movie".to_owned()),
        title: details.title,
        original_title: details.original_title,
        overview: details.overview,
        production_year: details.release_date.as_deref().and_then(parse_year),
        premiere_date: details.release_date,
        original_language: details.original_language,
        rating: details.vote_average,
        provider_ids: BTreeMap::from([("Tmdb".to_owned(), details.id.to_string())]),
        collection: details.belongs_to_collection.map(|collection| {
            crate::application::scraper::ScraperCollectionReference {
                provider_id: Some(collection.id.to_string()),
                name: collection.name,
            }
        }),
        ..ScraperMetadata::default()
    }
}

fn series_metadata_generic(details: TmdbSeriesDetails) -> ScraperMetadata {
    ScraperMetadata {
        item_type: Some("Series".to_owned()),
        title: details.name,
        original_title: details.original_name,
        overview: details.overview,
        production_year: details.first_air_date.as_deref().and_then(parse_year),
        premiere_date: details.first_air_date,
        end_date: details.last_air_date,
        status: details.status,
        original_language: details.original_language,
        rating: details.vote_average,
        provider_ids: BTreeMap::from([("Tmdb".to_owned(), details.id.to_string())]),
        ..ScraperMetadata::default()
    }
}

fn season_metadata_generic(details: TmdbSeasonDetails) -> ScraperMetadata {
    ScraperMetadata {
        item_type: Some("Season".to_owned()),
        title: details.name,
        overview: details.overview,
        premiere_date: details.air_date,
        provider_ids: BTreeMap::from([("Tmdb".to_owned(), details.id.to_string())]),
        ..ScraperMetadata::default()
    }
}

fn episode_metadata_generic(details: TmdbEpisodeDetails) -> ScraperMetadata {
    ScraperMetadata {
        item_type: Some("Episode".to_owned()),
        title: details.name,
        overview: details.overview,
        premiere_date: details.air_date,
        provider_ids: BTreeMap::from([("Tmdb".to_owned(), details.id.to_string())]),
        ..ScraperMetadata::default()
    }
}

fn collection_metadata_generic(details: TmdbCollectionDetails) -> ScraperMetadata {
    ScraperMetadata {
        item_type: Some("BoxSet".to_owned()),
        title: details.name,
        overview: details.overview,
        provider_ids: BTreeMap::from([("Tmdb".to_owned(), details.id.to_string())]),
        items: details
            .parts
            .into_iter()
            .map(|part| crate::application::scraper::ScraperMetadataItem {
                item_type: Some("Movie".to_owned()),
                title: part.title,
                production_year: part.release_date.as_deref().and_then(parse_year),
                provider_ids: BTreeMap::from([("Tmdb".to_owned(), part.id.to_string())]),
            })
            .collect(),
        ..ScraperMetadata::default()
    }
}

fn tmdb_images_generic(response: TmdbImagesResponse) -> ScraperImagesResponse {
    let mut images = Vec::new();
    append_tmdb_images(&mut images, response.posters, "Primary");
    append_tmdb_images(&mut images, response.backdrops, "Backdrop");
    append_tmdb_images(&mut images, response.stills, "Backdrop");
    append_tmdb_images(&mut images, response.logos, "Logo");
    append_tmdb_images(&mut images, response.profiles, "Profile");
    ScraperImagesResponse { images }
}

fn append_tmdb_images(
    target: &mut Vec<ScraperImage>,
    images: Vec<crate::application::tmdb::TmdbImageReference>,
    image_type: &str,
) {
    target.extend(images.into_iter().filter_map(|image| {
        let path = image.file_path?;
        let url = tmdb_image_url(&path);
        Some(ScraperImage {
            image_type: image_type.to_owned(),
            url: url.clone(),
            thumbnail_url: Some(url),
            language: image.iso_639_1,
            width: image.width,
            height: image.height,
            provider_name: Some("Tmdb".to_owned()),
        })
    }));
}

fn parse_year(value: &str) -> Option<i32> {
    value.get(..4)?.parse().ok()
}

fn tmdb_image_url(path: &str) -> String {
    format!("https://image.tmdb.org/t/p/w780{path}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::tmdb::TmdbImageReference;

    #[test]
    fn maps_season_posters_and_episode_stills_to_their_expected_orientations() {
        let images = tmdb_images_generic(TmdbImagesResponse {
            posters: vec![TmdbImageReference {
                file_path: Some("/season-poster.jpg".to_owned()),
                width: Some(1000),
                height: Some(1500),
                ..TmdbImageReference::default()
            }],
            stills: vec![TmdbImageReference {
                file_path: Some("/episode-still.jpg".to_owned()),
                width: Some(1920),
                height: Some(1080),
                ..TmdbImageReference::default()
            }],
            ..TmdbImagesResponse::default()
        });

        assert_eq!(images.images.len(), 2);
        assert_eq!(images.images[0].image_type, "Primary");
        assert_eq!(images.images[0].width, Some(1000));
        assert_eq!(images.images[0].height, Some(1500));
        assert_eq!(images.images[1].image_type, "Backdrop");
        assert_eq!(images.images[1].width, Some(1920));
        assert_eq!(images.images[1].height, Some(1080));
    }
}
