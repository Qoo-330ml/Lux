use serde::de::DeserializeOwned;
use serde_json::json;

use crate::application::{
    plugins::PluginService,
    tmdb::{
        TmdbClient, TmdbCollectionDetails, TmdbCreditsResponse, TmdbError, TmdbImagesResponse,
        TmdbMovieDetails, TmdbMovieSearchResponse, TmdbTvSearchResponse, fill_if_empty,
        localized_fields_complete, localized_tv_fields_complete, validate_id, validate_id_language,
        validate_search_response, validate_tv_search_response,
    },
};

#[derive(Clone)]
pub struct TmdbPluginClient {
    plugins: PluginService,
}

impl TmdbPluginClient {
    pub fn new(plugins: PluginService) -> Self {
        Self { plugins }
    }

    async fn request<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        params: Vec<(String, String)>,
    ) -> Result<T, TmdbError> {
        let value = self
            .plugins
            .call_tmdb(
                "tmdb.request",
                json!({ "endpoint": endpoint, "params": params }),
            )
            .await
            .map_err(|error| TmdbError::Transport(error.to_string()))?;
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
        }
        Ok(localized)
    }
}

#[derive(Clone)]
pub enum TmdbProvider {
    Direct(TmdbClient),
    Plugin(TmdbPluginClient),
}

impl From<TmdbClient> for TmdbProvider {
    fn from(client: TmdbClient) -> Self {
        Self::Direct(client)
    }
}

impl TmdbProvider {
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
        }
    }

    pub async fn set_api_key(&self, api_key: Option<&str>) {
        if let Self::Direct(client) = self {
            client.set_api_key(api_key).await;
        }
    }
}
