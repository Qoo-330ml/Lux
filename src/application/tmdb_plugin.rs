use std::collections::BTreeMap;

// TMDb endpoint conversion lives here. The provider-neutral contract is in
// application::scraper and is the only surface consumed by application services.
use crate::application::{
    scraper::{
        ScraperAdapter, ScraperCreditsResponse, ScraperError, ScraperExternalIdsResponse,
        ScraperFuture, ScraperGetRequest, ScraperImage, ScraperImageRequest, ScraperImagesResponse,
        ScraperItemType, ScraperMetadata, ScraperMetadataBundle, ScraperProvider,
        ScraperSearchRequest, ScraperSearchResponse, ScraperSearchResult, ScraperTrailer,
        ScraperTrailersResponse,
    },
    tmdb::{
        TmdbClient, TmdbCollectionDetails, TmdbEpisodeDetails, TmdbImagesResponse,
        TmdbMovieDetails, TmdbMovieSearchResponse, TmdbPersonDetails, TmdbSeasonDetails,
        TmdbSeriesDetails, TmdbTvSearchResponse,
    },
};

impl From<TmdbClient> for ScraperProvider {
    fn from(client: TmdbClient) -> Self {
        ScraperProvider::from_adapter(client)
    }
}

impl ScraperAdapter for TmdbClient {
    fn provider_key(&self) -> &str {
        "tmdb"
    }

    fn search(
        &self,
        request: ScraperSearchRequest,
    ) -> ScraperFuture<'_, Result<ScraperSearchResponse, ScraperError>> {
        Box::pin(direct_search_generic(self, request))
    }

    fn get(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperMetadata, ScraperError>> {
        Box::pin(direct_get_generic(self, request))
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
        Box::pin(direct_images_generic(self, request))
    }

    fn credits(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperCreditsResponse, ScraperError>> {
        Box::pin(direct_credits_generic(self, request))
    }

    fn external_ids(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperExternalIdsResponse, ScraperError>> {
        Box::pin(direct_external_ids_generic(self, request))
    }

    fn trailers(
        &self,
        request: ScraperGetRequest,
    ) -> ScraperFuture<'_, Result<ScraperTrailersResponse, ScraperError>> {
        Box::pin(direct_trailers_generic(self, request))
    }

    fn configure_api_key(&self, api_key: Option<String>) -> ScraperFuture<'_, ()> {
        Box::pin(async move {
            self.set_api_key(api_key.as_deref()).await;
        })
    }

    fn clear_response_cache(&self) -> ScraperFuture<'_, ()> {
        Box::pin(self.clear_response_cache())
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
