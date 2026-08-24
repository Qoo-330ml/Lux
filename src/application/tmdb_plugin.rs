use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::application::{
    plugins::{PluginService, TMDB_DYNAMIC_PLUGIN_ID},
    scraper::{
        ScraperCreditsResponse, ScraperError, ScraperExternalIdsResponse, ScraperGetRequest,
        ScraperImage, ScraperImageRequest, ScraperImagesResponse, ScraperItemType, ScraperMetadata,
        ScraperMetadataBundle, ScraperPluginClient, ScraperSearchRequest, ScraperSearchResponse,
        ScraperSearchResult, ScraperTrailer, ScraperTrailersResponse,
    },
    tmdb::{
        TmdbClient, TmdbCollectionDetails, TmdbCreditsResponse, TmdbEpisodeDetails, TmdbError,
        TmdbImagesResponse, TmdbMovieDetails, TmdbMovieSearchResponse, TmdbPersonDetails,
        TmdbSeasonDetails, TmdbSeriesDetails, TmdbTvSearchResponse, fill_if_empty,
        localized_fields_complete, localized_tv_fields_complete, validate_id, validate_id_language,
        validate_search_response, validate_tv_search_response,
    },
};

#[derive(Clone)]
pub struct TmdbPluginClient {
    scraper: ScraperPluginClient,
}

impl TmdbPluginClient {
    pub fn new(plugins: PluginService) -> Self {
        Self {
            scraper: ScraperPluginClient::new_with_provider_key(
                plugins.clone(),
                TMDB_DYNAMIC_PLUGIN_ID,
                "tmdb",
                plugins.provider_cache(),
            ),
        }
    }

    pub fn from_scraper(scraper: ScraperPluginClient) -> Self {
        Self { scraper }
    }

    pub fn provider_key(&self) -> &str {
        self.scraper.provider_key()
    }

    pub fn plugin_id(&self) -> &str {
        self.scraper.plugin_id()
    }

    pub(crate) async fn clear_response_cache(&self) {
        self.scraper.clear_response_cache().await;
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

    pub async fn bundle_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperMetadataBundle, ScraperError> {
        self.scraper.bundle(request).await
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

    pub async fn external_ids_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperExternalIdsResponse, ScraperError> {
        self.scraper.external_ids(request).await
    }

    pub async fn trailers_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperTrailersResponse, ScraperError> {
        self.scraper.trailers(request).await
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
        validate_id(item_id, item_type)?;
        let mut params = Vec::new();
        if !language.trim().is_empty() {
            params.push(("language".to_owned(), language.trim().to_owned()));
            params.push((
                "include_image_language".to_owned(),
                format!("{},en,null", language.trim()),
            ));
        }
        self.request(&format!("3/{item_type}/{item_id}/images"), params)
            .await
    }

    async fn season_images(
        &self,
        series_id: i64,
        season_number: i32,
        language: &str,
    ) -> Result<TmdbImagesResponse, TmdbError> {
        validate_id(series_id, "series")?;
        if !(-1..=1000).contains(&season_number) {
            return Err(TmdbError::InvalidRequest(
                "season number is out of range".to_owned(),
            ));
        }
        let mut params = Vec::new();
        if !language.trim().is_empty() {
            params.push(("language".to_owned(), language.trim().to_owned()));
            params.push((
                "include_image_language".to_owned(),
                format!("{},en,null", language.trim()),
            ));
        }
        self.request(
            &format!("3/tv/{series_id}/season/{season_number}/images"),
            params,
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
        validate_id(series_id, "series")?;
        if !(-1..=1000).contains(&season_number) || !(0..=10000).contains(&episode_number) {
            return Err(TmdbError::InvalidRequest(
                "episode number is out of range".to_owned(),
            ));
        }
        let mut params = Vec::new();
        if !language.trim().is_empty() {
            params.push(("language".to_owned(), language.trim().to_owned()));
            params.push((
                "include_image_language".to_owned(),
                format!("{},en,null", language.trim()),
            ));
        }
        self.request(
            &format!("3/tv/{series_id}/season/{season_number}/episode/{episode_number}/images"),
            params,
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
        ["3", "person", id] => (
            "metadata.get",
            json!({"itemType": "Person", "providerId": id, "language": language}),
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
        ["3", "person", _] => normalize_metadata(value, "Person"),
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
    let genres = metadata
        .get("Genres")
        .or_else(|| metadata.get("genres"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|name| json!({"id": 0, "name": name}))
        .collect::<Vec<_>>();
    let countries = metadata
        .get("Countries")
        .or_else(|| metadata.get("countries"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|name| json!({"iso_3166_1": null, "name": name}))
        .collect::<Vec<_>>();
    let companies = metadata
        .get("Studios")
        .or_else(|| metadata.get("studios"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|name| json!({"id": 0, "name": name}))
        .collect::<Vec<_>>();
    Ok(json!({
        "id": provider_id,
        "title": metadata.get("Name").cloned().unwrap_or(Value::Null),
        "original_title": metadata.get("OriginalTitle").cloned().unwrap_or(Value::Null),
        "overview": metadata.get("Overview").cloned().unwrap_or(Value::Null),
        "birthday": metadata.get("Birthday").cloned().unwrap_or(Value::Null),
        "deathday": metadata.get("Deathday").cloned().unwrap_or(Value::Null),
        "known_for_department": metadata
            .get("KnownForDepartment")
            .cloned()
            .unwrap_or(Value::Null),
        "place_of_birth": metadata
            .get("PlaceOfBirth")
            .cloned()
            .unwrap_or(Value::Null),
        "tagline": metadata.get("Tagline").cloned().unwrap_or(Value::Null),
        "homepage": metadata.get("Website").cloned().unwrap_or(Value::Null),
        "premiere_date": premiere_date,
        "last_air_date": metadata.get("EndDate").cloned().unwrap_or(Value::Null),
        "status": metadata.get("Status").cloned().unwrap_or(Value::Null),
        "original_language": metadata.get("OriginalLanguage").cloned().unwrap_or(Value::Null),
        "vote_average": metadata.get("Rating").cloned().unwrap_or(Value::Null),
        "vote_count": metadata.get("Votes").cloned().unwrap_or(Value::Null),
        "runtime": metadata.get("Runtime").cloned().unwrap_or(Value::Null),
        "certification": metadata.get("OfficialRating").cloned().unwrap_or(Value::Null),
        "genres": genres,
        "production_countries": countries,
        "production_companies": companies,
        "poster_path": image_path(metadata.get("PosterUrl")),
        "backdrop_path": image_path(metadata.get("BackdropUrl")),
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
    let crew = value
        .get("crew")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|credit| {
            Ok(json!({
                "id": provider_id(&credit)?,
                "name": credit.get("Name").cloned().unwrap_or(Value::Null),
                "job": credit.get("Job").cloned().unwrap_or(Value::Null),
                "department": credit.get("Department").cloned().unwrap_or(Value::Null),
                "profile_path": image_path(credit.get("ProfileUrl")),
            }))
        })
        .collect::<Result<Vec<_>, TmdbError>>()?;
    Ok(json!({"cast": cast, "crew": crew}))
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

/// Provider-neutral application boundary for the selected metadata scraper.
#[derive(Clone)]
pub enum ScraperProvider {
    Direct(TmdbClient),
    Plugin(TmdbPluginClient),
    Generic(ScraperPluginClient),
}

impl From<TmdbClient> for ScraperProvider {
    fn from(client: TmdbClient) -> Self {
        Self::Direct(client)
    }
}

impl ScraperProvider {
    pub fn plugin_id(&self) -> Option<&str> {
        match self {
            Self::Direct(_) => None,
            Self::Plugin(client) => Some(client.plugin_id()),
            Self::Generic(client) => Some(client.plugin_id()),
        }
    }

    pub fn provider_key(&self) -> &str {
        match self {
            Self::Direct(_) => "tmdb",
            Self::Plugin(client) => client.provider_key(),
            Self::Generic(client) => client.provider_key(),
        }
    }

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
        result.selected_provider_entry(self.provider_key())
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

    pub async fn bundle_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperMetadataBundle, ScraperError> {
        match self {
            Self::Direct(_) => Err(ScraperError::Provider(
                "metadata bundle is not available for the direct provider".to_owned(),
            )),
            Self::Plugin(client) => client.bundle_generic(request).await,
            Self::Generic(client) => client.bundle(request).await,
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

    pub async fn external_ids_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperExternalIdsResponse, ScraperError> {
        match self {
            Self::Direct(client) => direct_external_ids_generic(client, request).await,
            Self::Plugin(client) => client.external_ids_generic(request).await,
            Self::Generic(client) => client.external_ids(request).await,
        }
    }

    pub async fn trailers_generic(
        &self,
        request: ScraperGetRequest,
    ) -> Result<ScraperTrailersResponse, ScraperError> {
        match self {
            Self::Direct(client) => direct_trailers_generic(client, request).await,
            Self::Plugin(client) => client.trailers_generic(request).await,
            Self::Generic(client) => client.trailers(request).await,
        }
    }

    pub async fn set_api_key(&self, api_key: Option<&str>) {
        if let Self::Direct(client) = self {
            client.set_api_key(api_key).await;
        }
    }

    pub(crate) async fn clear_response_cache(&self) {
        match self {
            Self::Direct(client) => client.clear_response_cache().await,
            Self::Plugin(client) => client.clear_response_cache().await,
            Self::Generic(client) => client.clear_response_cache().await,
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
        ScraperItemType::Movie => {
            direct_movie_metadata_generic(client, id, &request.language).await
        }
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
        ScraperItemType::Person => client
            .person_details(id, &request.language)
            .await
            .map(person_metadata_generic)
            .map_err(|error| ScraperError::Provider(error.to_string())),
    }
}

async fn direct_movie_metadata_generic(
    client: &TmdbClient,
    id: i64,
    language: &str,
) -> Result<ScraperMetadata, ScraperError> {
    let mut details = client
        .movie_details(id, language)
        .await
        .map_err(|error| ScraperError::Provider(error.to_string()))?;
    let preferred_region = if language.trim().starts_with("zh") {
        "CN"
    } else {
        "US"
    };
    if let Ok(release_dates) = client.movie_release_dates(id).await {
        details.certification = release_dates
            .certification(preferred_region)
            .map(str::to_owned);
    }
    Ok(movie_metadata_generic(details))
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
        crew: response
            .crew
            .into_iter()
            .map(|credit| crate::application::scraper::ScraperCrewCredit {
                provider_id: credit.id.to_string(),
                name: credit.name,
                job: credit.job,
                department: credit.department,
            })
            .collect(),
    })
}

async fn direct_external_ids_generic(
    client: &TmdbClient,
    request: ScraperGetRequest,
) -> Result<ScraperExternalIdsResponse, ScraperError> {
    let id = request
        .provider_id
        .parse::<i64>()
        .map_err(|_| ScraperError::Provider("TMDb provider ID is invalid".to_owned()))?;
    let ids = match request.item_type {
        ScraperItemType::Movie => client.movie_external_ids(id).await,
        ScraperItemType::Series => client.tv_external_ids(id).await,
        ScraperItemType::Person => client.person_external_ids(id).await,
        item_type => {
            return Err(ScraperError::Provider(format!(
                "TMDb direct scraper does not support external IDs for {}",
                item_type.as_str()
            )));
        }
    }
    .map_err(|error| ScraperError::Provider(error.to_string()))?;
    let mut provider_ids = BTreeMap::from([("Tmdb".to_owned(), id.to_string())]);
    if let Some(imdb_id) = ids.imdb_id {
        provider_ids.insert("Imdb".to_owned(), imdb_id);
    }
    if let Some(tvdb_id) = ids.tvdb_id {
        provider_ids.insert("Tvdb".to_owned(), tvdb_id.to_string());
    }
    if let Some(wikidata_id) = ids.wikidata_id {
        provider_ids.insert("Wikidata".to_owned(), wikidata_id);
    }
    Ok(ScraperExternalIdsResponse { provider_ids })
}

async fn direct_trailers_generic(
    client: &TmdbClient,
    request: ScraperGetRequest,
) -> Result<ScraperTrailersResponse, ScraperError> {
    let id = request
        .provider_id
        .parse::<i64>()
        .map_err(|_| ScraperError::Provider("TMDb provider ID is invalid".to_owned()))?;
    let videos = match request.item_type {
        ScraperItemType::Movie => client.movie_videos(id, &request.language).await,
        ScraperItemType::Series => client.tv_videos(id, &request.language).await,
        item_type => {
            return Err(ScraperError::Provider(format!(
                "TMDb direct scraper does not support trailers for {}",
                item_type.as_str()
            )));
        }
    }
    .map_err(|error| ScraperError::Provider(error.to_string()))?;
    Ok(ScraperTrailersResponse {
        trailers: videos
            .results
            .into_iter()
            .filter_map(|video| {
                let key = video.key?;
                let url = match video.site.as_deref()? {
                    "YouTube" => format!("https://www.youtube.com/watch?v={key}"),
                    "Vimeo" => format!("https://vimeo.com/{key}"),
                    _ => return None,
                };
                Some(ScraperTrailer {
                    name: video.name,
                    url: Some(url),
                    video_type: video.video_type,
                    official: video.official,
                    published_at: video.published_at,
                })
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
    let set_name = details
        .belongs_to_collection
        .as_ref()
        .and_then(|collection| collection.name.clone());
    let set_id = details
        .belongs_to_collection
        .as_ref()
        .map(|collection| collection.id.to_string());
    ScraperMetadata {
        item_type: Some("Movie".to_owned()),
        title: details.title,
        original_title: details.original_title,
        overview: details.overview,
        tagline: details.tagline,
        website: details.homepage,
        production_year: details.release_date.as_deref().and_then(parse_year),
        premiere_date: details.release_date,
        status: details.status,
        original_language: details.original_language,
        rating: details.vote_average,
        votes: details.vote_count,
        runtime: details.runtime,
        certification: details.certification,
        set_name,
        set_id,
        poster_url: details.poster_path.map(|path| tmdb_image_url(&path)),
        backdrop_url: details.backdrop_path.map(|path| tmdb_image_url(&path)),
        genres: details
            .genres
            .into_iter()
            .filter_map(|genre| genre.name)
            .collect(),
        countries: details
            .production_countries
            .into_iter()
            .filter_map(|country| country.name)
            .collect(),
        studios: details
            .production_companies
            .into_iter()
            .filter_map(|company| company.name)
            .collect(),
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

fn person_metadata_generic(details: TmdbPersonDetails) -> ScraperMetadata {
    ScraperMetadata {
        item_type: Some("Person".to_owned()),
        title: details.name,
        overview: details.biography,
        birthday: details.birthday,
        deathday: details.deathday,
        known_for_department: details.known_for_department,
        place_of_birth: details.place_of_birth,
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

    #[test]
    fn maps_person_details_to_scraper_metadata() {
        let metadata = person_metadata_generic(crate::application::tmdb::TmdbPersonDetails {
            id: 9,
            name: Some("演员甲".to_owned()),
            biography: Some("人物简介".to_owned()),
            birthday: Some("1970-01-01".to_owned()),
            deathday: Some("2020-01-01".to_owned()),
            known_for_department: Some("Acting".to_owned()),
            place_of_birth: Some("测试城市".to_owned()),
            profile_path: Some("/profile.jpg".to_owned()),
        });

        assert_eq!(metadata.item_type.as_deref(), Some("Person"));
        assert_eq!(metadata.title.as_deref(), Some("演员甲"));
        assert_eq!(metadata.overview.as_deref(), Some("人物简介"));
        assert_eq!(metadata.birthday.as_deref(), Some("1970-01-01"));
        assert_eq!(metadata.deathday.as_deref(), Some("2020-01-01"));
        assert_eq!(metadata.known_for_department.as_deref(), Some("Acting"));
        assert_eq!(metadata.place_of_birth.as_deref(), Some("测试城市"));
    }
}
