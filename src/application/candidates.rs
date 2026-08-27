use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{sync::Semaphore, task::JoinSet};

use crate::{
    application::{
        home::HomeService,
        images::{ImageWriteError, ImageWriteService, image_no_candidate_key},
        media_matching::{MediaKind, parse_media_name, title_candidates},
        metadata::{MetadataCandidate, MetadataField, MetadataSource, MetadataState, NfoMetadata},
        nfo::{MovieNfoCredit, MovieNfoMetadata, NfoWriteError, NfoWriteService},
        people::{ActorCredit, PeopleError},
        scraper::{
            ScraperError, ScraperGetRequest, ScraperImageRequest, ScraperItemType, ScraperMetadata,
            ScraperProvider, ScraperSearchResponse, ScraperSearchResult, provider_id_for_key,
            provider_key_from_plugin_id,
        },
    },
    observability::resources::ResourceMetrics,
    storage::{
        Database, MetadataCapabilityResult, NewMetadataCandidate, SelectedMetadataUpdate,
        StorageError, StoredMediaMetadata, StoredMetadataCandidate,
        StoredMetadataCapabilityAttempt,
    },
};

const MAX_MOVIE_NFO_ACTORS: usize = 30;
const ACTOR_METADATA_FETCH_CONCURRENCY: usize = 4;
const IMAGE_ITEM_CONCURRENCY: usize = 4;
const SCRAPER_IMAGE_TYPES: [&str; 8] = [
    "POSTER",
    "FANART",
    "LOGO",
    "THUMB",
    "BANNER",
    "DISC",
    "ART",
    "WALLPAPER",
];
const CAPABILITY_CREDITS: &str = "CREDITS";
const CAPABILITY_EXTERNAL_IDS: &str = "EXTERNAL_IDS";
const CAPABILITY_TRAILERS: &str = "TRAILERS";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MetadataRequestPlan {
    pub(crate) needs_metadata: bool,
    pub(crate) needs_images: bool,
    pub(crate) needs_credits: bool,
    pub(crate) needs_external_ids: bool,
    pub(crate) needs_trailers: bool,
    pub(crate) image_policy: Option<ImageSelectionPolicy>,
}

impl MetadataRequestPlan {
    pub(crate) const fn full() -> Self {
        Self {
            needs_metadata: true,
            needs_images: true,
            needs_credits: true,
            needs_external_ids: true,
            needs_trailers: true,
            image_policy: None,
        }
    }

    fn capability_count(self) -> usize {
        [
            self.needs_metadata,
            self.needs_images,
            self.needs_credits,
            self.needs_external_ids,
            self.needs_trailers,
        ]
        .into_iter()
        .filter(|needed| *needed)
        .count()
    }
}

fn metadata_request_plan(
    current: &StoredMediaMetadata,
    images_missing: bool,
    credits_missing: bool,
    details: Option<&crate::application::nfo::LocalNfoDetails>,
) -> MetadataRequestPlan {
    let Some(fields) = fill_missing_fields(&current.item_type) else {
        return MetadataRequestPlan::full();
    };
    let state = metadata_state(current);
    MetadataRequestPlan {
        needs_metadata: !state.has_complete_fill_values(fields)
            || !fill_missing_scalar_values_complete(current),
        needs_images: images_missing,
        needs_credits: credits_missing,
        needs_external_ids: current.item_type == "MOVIE" && !has_complete_external_ids(current),
        needs_trailers: matches!(current.item_type.as_str(), "MOVIE" | "SERIES")
            && details.is_none_or(|details| details.trailers.is_empty()),
        image_policy: None,
    }
}

fn credits_are_missing(
    actor_relation_exists: bool,
    details: Option<&crate::application::nfo::LocalNfoDetails>,
) -> bool {
    !actor_relation_exists
        || details.is_none_or(|value| value.directors.is_empty() || value.writers.is_empty())
}

fn has_complete_external_ids(current: &StoredMediaMetadata) -> bool {
    let Some(raw) = current.provider_ids_json.as_deref() else {
        return false;
    };
    serde_json::from_str::<BTreeMap<String, String>>(raw)
        .ok()
        .is_some_and(|ids| ids.values().filter(|id| !id.trim().is_empty()).count() > 1)
}

#[derive(Clone)]
pub struct MetadataCandidateService {
    database: Database,
}

impl MetadataCandidateService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn list_pending(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        let total = self.database.count_pending_metadata_candidates().await?;
        let rows = self
            .database
            .list_pending_metadata_candidates(offset, limit)
            .await?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let current = self.database.find_media_item_metadata(&row.item_id).await?;
            items.push(candidate_view(row, current.as_ref())?);
        }
        Ok(MetadataCandidatePage {
            items,
            total,
            offset,
            limit,
        })
    }

    pub async fn list_for_item(
        &self,
        item_id: &str,
        search: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        let current = self
            .database
            .find_media_item_metadata(item_id)
            .await?
            .ok_or(MetadataCandidateError::ItemNotFound)?;
        let search = search.map(str::trim).filter(|value| !value.is_empty());
        if search.is_some_and(|value| value.chars().count() > 128) {
            return Err(MetadataCandidateError::InvalidSearch);
        }
        let total = self
            .database
            .count_pending_metadata_candidates_for_item(item_id, search)
            .await?;
        let rows = self
            .database
            .list_pending_metadata_candidates_for_item(item_id, search, offset, limit)
            .await?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            items.push(candidate_view(row, Some(&current))?);
        }
        Ok(MetadataCandidatePage {
            items,
            total,
            offset,
            limit,
        })
    }

    pub async fn search_and_store(
        &self,
        item_id: &str,
        query: &str,
        year: Option<i32>,
        scraper: &ScraperProvider,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        self.search_and_store_with_mode(
            item_id,
            query,
            year,
            scraper,
            CandidateSearchMode::Manual,
            MetadataRequestPlan::full(),
        )
        .await
    }

    pub async fn search_and_store_for_automatic_match(
        &self,
        item_id: &str,
        query: &str,
        year: Option<i32>,
        scraper: &ScraperProvider,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        self.search_and_store_for_automatic_match_with_plan(
            item_id,
            query,
            year,
            scraper,
            MetadataRequestPlan::full(),
        )
        .await
    }

    pub(crate) async fn search_and_store_for_automatic_match_with_plan(
        &self,
        item_id: &str,
        query: &str,
        year: Option<i32>,
        scraper: &ScraperProvider,
        plan: MetadataRequestPlan,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        self.search_and_store_with_mode(
            item_id,
            query,
            year,
            scraper,
            CandidateSearchMode::AutomaticReuse,
            plan,
        )
        .await
    }

    pub(crate) async fn search_and_store_for_automatic_match_fresh(
        &self,
        item_id: &str,
        query: &str,
        year: Option<i32>,
        scraper: &ScraperProvider,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        self.search_and_store_with_mode(
            item_id,
            query,
            year,
            scraper,
            CandidateSearchMode::AutomaticFresh,
            MetadataRequestPlan::full(),
        )
        .await
    }

    async fn search_and_store_with_mode(
        &self,
        item_id: &str,
        query: &str,
        year: Option<i32>,
        scraper: &ScraperProvider,
        mode: CandidateSearchMode,
        plan: MetadataRequestPlan,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        let current = self
            .database
            .find_media_item_metadata(item_id)
            .await?
            .ok_or(MetadataCandidateError::ItemNotFound)?;
        if matches!(mode, CandidateSearchMode::AutomaticReuse)
            && let Some(page) = self
                .reuse_unexpired_automatic_candidates(item_id, &current, scraper, plan)
                .await?
        {
            return Ok(page);
        }
        let kind = match current.item_type.as_str() {
            "MOVIE" => MediaKind::Movie,
            "SERIES" => MediaKind::Series,
            "SEASON" | "EPISODE" => {
                return self
                    .search_child_and_store(item_id, query, year, &current, scraper, plan)
                    .await;
            }
            _ => return Err(MetadataCandidateError::InvalidSearch),
        };
        let parsed = parse_media_name(query, kind);
        let query = parsed
            .as_ref()
            .map(|value| value.title.as_str())
            .unwrap_or_else(|| query.trim());
        let year = year.or_else(|| parsed.as_ref().and_then(|value| value.production_year));
        if query.is_empty() || query.chars().count() > 128 {
            return Err(MetadataCandidateError::InvalidSearch);
        }
        if year.is_some_and(|value| !(1800..=2200).contains(&value)) {
            return Err(MetadataCandidateError::InvalidSearch);
        }

        let item_type = match kind {
            MediaKind::Movie => crate::application::scraper::ScraperItemType::Movie,
            MediaKind::Series => crate::application::scraper::ScraperItemType::Series,
            MediaKind::Episode => return Err(MetadataCandidateError::InvalidSearch),
        };
        let direct_provider_id = selected_scraper_provider_id(&current, scraper).filter(|_| {
            if plan != MetadataRequestPlan::full() {
                return true;
            }
            let same_title = crate::application::media_matching::normalize_title(query)
                == crate::application::media_matching::normalize_title(&current.title);
            let same_year =
                year.is_none_or(|year| current.production_year == Some(i64::from(year)));
            same_title && same_year
        });
        let (response, direct_details) = if let Some(provider_id) = direct_provider_id.as_deref() {
            let details = if plan.needs_metadata || plan == MetadataRequestPlan::full() {
                Some(
                    scraper
                        .get_generic(ScraperGetRequest::new(item_type, provider_id, "zh-CN"))
                        .await
                        .map_err(MetadataCandidateError::Scraper)?,
                )
            } else {
                None
            };
            let mut provider_ids = current_provider_ids(&current);
            if let Some(details) = details.as_ref() {
                provider_ids.extend(details.provider_ids.clone());
            }
            provider_ids
                .entry(scraper.provider_key().to_owned())
                .or_insert_with(|| provider_id.to_owned());
            (
                ScraperSearchResponse {
                    items: vec![ScraperSearchResult {
                        item_type: Some(item_type.as_str().to_owned()),
                        title: details
                            .as_ref()
                            .and_then(|value| value.title.clone())
                            .or_else(|| Some(current.title.clone())),
                        original_title: details
                            .as_ref()
                            .and_then(|value| value.original_title.clone())
                            .or_else(|| current.original_title.clone()),
                        overview: details
                            .as_ref()
                            .and_then(|value| value.overview.clone())
                            .or_else(|| current.overview.clone()),
                        production_year: details
                            .as_ref()
                            .and_then(|value| value.production_year)
                            .or_else(|| {
                                current
                                    .production_year
                                    .and_then(|value| i32::try_from(value).ok())
                            }),
                        premiere_date: details
                            .as_ref()
                            .and_then(|value| value.premiere_date.clone())
                            .or_else(|| current.premiere_date.clone()),
                        original_language: details
                            .as_ref()
                            .and_then(|value| value.original_language.clone())
                            .or_else(|| current.original_language.clone()),
                        provider_ids,
                        ..ScraperSearchResult::default()
                    }],
                },
                details,
            )
        } else {
            (
                search_generic(scraper, item_type, query, year)
                    .await
                    .map_err(MetadataCandidateError::Scraper)?,
                None,
            )
        };
        let expires_at = candidate_expiry();
        let mut results = response.items.into_iter().take(20).collect::<Vec<_>>();
        if matches!(
            mode,
            CandidateSearchMode::AutomaticReuse | CandidateSearchMode::AutomaticFresh
        ) {
            results.sort_by(|left, right| {
                search_result_score(&current, left)
                    .total_cmp(&search_result_score(&current, right))
                    .reverse()
            });
            results.truncate(2);
        }
        for (result_index, result) in results.into_iter().enumerate() {
            let Some((provider, provider_id)) = scraper.selected_provider_entry(&result) else {
                continue;
            };
            let provider = provider.to_owned();
            let provider_id = provider_id.to_owned();
            if matches!(
                mode,
                CandidateSearchMode::AutomaticReuse | CandidateSearchMode::AutomaticFresh
            ) && result_index > 0
            {
                let score = search_result_score(&current, &result);
                let mut provider_ids = result.provider_ids.clone();
                provider_ids
                    .entry(scraper.provider_key().to_owned())
                    .or_insert_with(|| provider_id.clone());
                self.store_candidate(
                    item_id,
                    &current,
                    CandidateMetadata {
                        title: result
                            .title
                            .clone()
                            .or_else(|| result.original_title.clone())
                            .unwrap_or_else(|| query.to_owned()),
                        original_title: result.original_title,
                        overview: result.overview,
                        tagline: None,
                        website: None,
                        release_date: result.premiere_date,
                        end_date: None,
                        status: None,
                        set_name: None,
                        set_id: None,
                        poster_url: result.image_url,
                        backdrop_url: result.backdrop_image_url,
                        production_year: result.production_year,
                        rating: result.rating,
                        original_language: result.original_language,
                        runtime: None,
                        votes: None,
                        certification: None,
                        countries: Vec::new(),
                        genres: Vec::new(),
                        studios: Vec::new(),
                        provider_ids,
                        directors: Vec::new(),
                        writers: Vec::new(),
                        trailers: Vec::new(),
                        provider,
                        provider_id,
                        images: BTreeMap::new(),
                        actors: Vec::new(),
                        metadata_fetched: false,
                        score: Some(score),
                    },
                    expires_at,
                )
                .await?;
                continue;
            }
            let bundle = if direct_details.is_none()
                && plan.capability_count() > 1
                && matches!(
                    item_type,
                    crate::application::scraper::ScraperItemType::Movie
                        | crate::application::scraper::ScraperItemType::Series
                ) {
                scraper
                    .bundle_generic(ScraperGetRequest::new(
                        item_type,
                        provider_id.clone(),
                        "zh-CN",
                    ))
                    .await
                    .ok()
            } else {
                None
            };
            let details = if direct_details.is_some() {
                direct_details.clone()
            } else if let Some(bundle) = bundle.as_ref() {
                Some(bundle.metadata.clone())
            } else if plan.needs_metadata {
                scraper
                    .get_generic(crate::application::scraper::ScraperGetRequest::new(
                        item_type,
                        provider_id.clone(),
                        "zh-CN",
                    ))
                    .await
                    .ok()
            } else {
                None
            };
            let title = result
                .title
                .clone()
                .or_else(|| details.as_ref().and_then(|value| value.title.clone()))
                .or_else(|| result.original_title.clone())
                .unwrap_or_else(|| query.to_owned());
            let mut capability_results = Vec::new();
            let mut capability_failures = Vec::new();
            let (images, credits, external_ids, trailers, images_response) = if let Some(bundle) =
                bundle
            {
                let bundle_images = bundle.images;
                capability_results.extend([
                    MetadataCapabilityResult {
                        capability: CAPABILITY_CREDITS,
                        has_data: !bundle.credits.cast.is_empty()
                            || !bundle.credits.crew.is_empty(),
                    },
                    MetadataCapabilityResult {
                        capability: CAPABILITY_EXTERNAL_IDS,
                        has_data: !bundle.external_ids.provider_ids.is_empty(),
                    },
                    MetadataCapabilityResult {
                        capability: CAPABILITY_TRAILERS,
                        has_data: bundle.trailers.trailers.iter().any(|trailer| {
                            trailer.url.as_deref().is_some_and(|url| !url.is_empty())
                        }),
                    },
                ]);
                (
                    bundle_images.clone(),
                    bundle.credits,
                    Some(bundle.external_ids),
                    bundle
                        .trailers
                        .trailers
                        .into_iter()
                        .filter_map(|trailer| trailer.url)
                        .collect(),
                    Some(bundle_images),
                )
            } else {
                let (images_response, credits_result, external_ids_result, trailers_result) = tokio::join!(
                    async {
                        if plan.needs_images {
                            scraper
                                .images_generic(
                                    crate::application::scraper::ScraperImageRequest::new(
                                        item_type,
                                        provider_id.clone(),
                                        "zh-CN",
                                    ),
                                )
                                .await
                                .ok()
                        } else {
                            None
                        }
                    },
                    async {
                        if plan.needs_credits
                            && matches!(
                                item_type,
                                crate::application::scraper::ScraperItemType::Movie
                                    | crate::application::scraper::ScraperItemType::Series
                            )
                        {
                            Some(
                                scraper
                                    .credits_generic(
                                        crate::application::scraper::ScraperGetRequest::new(
                                            item_type,
                                            provider_id.clone(),
                                            "zh-CN",
                                        ),
                                    )
                                    .await,
                            )
                        } else {
                            None
                        }
                    },
                    async {
                        if plan.needs_external_ids && item_type == ScraperItemType::Movie {
                            Some(
                                scraper
                                    .external_ids_generic(ScraperGetRequest::new(
                                        item_type,
                                        provider_id.clone(),
                                        "zh-CN",
                                    ))
                                    .await,
                            )
                        } else {
                            None
                        }
                    },
                    async {
                        if plan.needs_trailers
                            && matches!(item_type, ScraperItemType::Movie | ScraperItemType::Series)
                        {
                            Some(
                                scraper
                                    .trailers_generic(ScraperGetRequest::new(
                                        item_type,
                                        provider_id.clone(),
                                        "zh-CN",
                                    ))
                                    .await,
                            )
                        } else {
                            None
                        }
                    }
                );
                let credits = match credits_result {
                    Some(Ok(value)) => {
                        capability_results.push(MetadataCapabilityResult {
                            capability: CAPABILITY_CREDITS,
                            has_data: !value.cast.is_empty() || !value.crew.is_empty(),
                        });
                        value
                    }
                    Some(Err(error)) => {
                        capability_failures.push((CAPABILITY_CREDITS, error));
                        crate::application::scraper::ScraperCreditsResponse::default()
                    }
                    None => crate::application::scraper::ScraperCreditsResponse::default(),
                };
                let external_ids = match external_ids_result {
                    Some(Ok(value)) => {
                        capability_results.push(MetadataCapabilityResult {
                            capability: CAPABILITY_EXTERNAL_IDS,
                            has_data: !value.provider_ids.is_empty(),
                        });
                        Some(value)
                    }
                    Some(Err(error)) => {
                        capability_failures.push((CAPABILITY_EXTERNAL_IDS, error));
                        None
                    }
                    None => None,
                };
                let trailers = match trailers_result {
                    Some(Ok(response)) => {
                        let trailers = response
                            .trailers
                            .into_iter()
                            .filter_map(|trailer| trailer.url)
                            .collect::<Vec<_>>();
                        capability_results.push(MetadataCapabilityResult {
                            capability: CAPABILITY_TRAILERS,
                            has_data: !trailers.is_empty(),
                        });
                        trailers
                    }
                    Some(Err(error)) => {
                        capability_failures.push((CAPABILITY_TRAILERS, error));
                        Vec::new()
                    }
                    None => Vec::new(),
                };
                (
                    images_response.clone().unwrap_or_default(),
                    credits,
                    external_ids,
                    trailers,
                    images_response,
                )
            };
            let now = current_unix_timestamp();
            self.database
                .record_metadata_capability_results(
                    item_id,
                    scraper.provider_key(),
                    &provider_id,
                    &capability_results,
                    now,
                )
                .await
                .map_err(MetadataCandidateError::Storage)?;
            for (capability, error) in capability_failures {
                if capability_error_is_permanent(&error) {
                    self.database
                        .record_metadata_capability_results(
                            item_id,
                            scraper.provider_key(),
                            &provider_id,
                            &[MetadataCapabilityResult {
                                capability,
                                has_data: false,
                            }],
                            now,
                        )
                        .await
                        .map_err(MetadataCandidateError::Storage)?;
                } else {
                    self.database
                        .record_metadata_capability_failure(
                            item_id,
                            scraper.provider_key(),
                            &provider_id,
                            capability,
                            now,
                        )
                        .await
                        .map_err(MetadataCandidateError::Storage)?;
                }
            }
            if plan.needs_images
                && images_response
                    .as_ref()
                    .is_some_and(|response| response.images.is_empty())
            {
                self.record_explicitly_unavailable_images(item_id, scraper, &provider_id)
                    .await?;
            }
            let actors = generic_candidate_actors(&credits.cast);
            let mut provider_ids = details
                .as_ref()
                .map(|value| value.provider_ids.clone())
                .unwrap_or_default();
            provider_ids
                .entry(scraper.provider_key().to_owned())
                .or_insert_with(|| provider_id.clone());
            if let Some(external_ids) = external_ids {
                provider_ids.extend(external_ids.provider_ids);
            }
            self.store_candidate(
                item_id,
                &current,
                CandidateMetadata {
                    title,
                    original_title: details
                        .as_ref()
                        .and_then(|value| value.original_title.clone())
                        .or(result.original_title),
                    overview: details
                        .as_ref()
                        .and_then(|value| value.overview.clone())
                        .or(result.overview),
                    tagline: details.as_ref().and_then(|value| value.tagline.clone()),
                    website: details.as_ref().and_then(|value| value.website.clone()),
                    release_date: details
                        .as_ref()
                        .and_then(|value| value.premiere_date.clone())
                        .or(result.premiere_date),
                    end_date: details.as_ref().and_then(|value| value.end_date.clone()),
                    status: details.as_ref().and_then(|value| value.status.clone()),
                    set_name: details.as_ref().and_then(|value| value.set_name.clone()),
                    set_id: details.as_ref().and_then(|value| value.set_id.clone()),
                    poster_url: details.as_ref().and_then(|value| value.poster_url.clone()),
                    backdrop_url: details
                        .as_ref()
                        .and_then(|value| value.backdrop_url.clone()),
                    production_year: details
                        .as_ref()
                        .and_then(|value| value.production_year)
                        .or(result.production_year),
                    rating: details
                        .as_ref()
                        .and_then(|value| value.rating)
                        .or(result.rating),
                    original_language: details
                        .as_ref()
                        .and_then(|value| value.original_language.clone())
                        .or(result.original_language),
                    runtime: details.as_ref().and_then(|value| value.runtime),
                    votes: details.as_ref().and_then(|value| value.votes),
                    certification: details
                        .as_ref()
                        .and_then(|value| value.certification.clone()),
                    countries: details
                        .as_ref()
                        .map(|value| value.countries.clone())
                        .unwrap_or_default(),
                    genres: details
                        .as_ref()
                        .map(|value| value.genres.clone())
                        .unwrap_or_default(),
                    studios: details
                        .as_ref()
                        .map(|value| value.studios.clone())
                        .unwrap_or_default(),
                    provider_ids,
                    directors: generic_candidate_crew(&credits.crew, CrewRole::Director),
                    writers: generic_candidate_crew(&credits.crew, CrewRole::Writer),
                    trailers,
                    provider,
                    provider_id,
                    images: generic_candidate_images(&images.images, item_type),
                    actors,
                    metadata_fetched: details.is_some(),
                    score: direct_provider_id.as_ref().map(|_| 100.0),
                },
                expires_at,
            )
            .await?;
        }
        self.list_for_item(item_id, None, 0, 50).await
    }

    async fn reuse_unexpired_automatic_candidates(
        &self,
        item_id: &str,
        current: &StoredMediaMetadata,
        scraper: &ScraperProvider,
        plan: MetadataRequestPlan,
    ) -> Result<Option<MetadataCandidatePage>, MetadataCandidateError> {
        let provider_key = provider_key_from_plugin_id(scraper.provider_key());
        let mut rows = self
            .database
            .list_unexpired_pending_metadata_candidates_for_item(item_id, 50)
            .await?;
        rows.retain(|row| provider_key_from_plugin_id(&row.provider) == provider_key);
        rows.truncate(2);
        let Some(best) = rows.first_mut() else {
            return Ok(None);
        };
        if plan.needs_metadata && !Self::candidate_metadata_was_fetched(&best.candidate_json)? {
            let item_type = match current.item_type.as_str() {
                "MOVIE" => ScraperItemType::Movie,
                "SERIES" => ScraperItemType::Series,
                _ => return Ok(None),
            };
            let details = scraper
                .get_generic(ScraperGetRequest::new(
                    item_type,
                    best.provider_id.clone(),
                    "zh-CN",
                ))
                .await
                .map_err(MetadataCandidateError::Scraper)?;
            let candidate_json =
                Self::merge_scraper_metadata_into_candidate(&best.candidate_json, &details)?;
            if !self
                .database
                .update_pending_metadata_candidate_json(item_id, &best.id, &candidate_json)
                .await?
            {
                return Ok(None);
            }
            best.candidate_json = candidate_json;
        }
        let items = rows
            .into_iter()
            .map(|row| candidate_view(row, Some(current)))
            .collect::<Result<Vec<_>, _>>()?;
        let total = i64::try_from(items.len()).unwrap_or(i64::MAX);
        Ok(Some(MetadataCandidatePage {
            items,
            total,
            offset: 0,
            limit: 50,
        }))
    }

    fn candidate_metadata_was_fetched(
        candidate_json: &str,
    ) -> Result<bool, MetadataCandidateError> {
        let value = serde_json::from_str::<Value>(candidate_json)
            .map_err(|error| MetadataCandidateError::InvalidCandidateJson(error.to_string()))?;
        Ok(value
            .get("metadataFetched")
            .and_then(Value::as_bool)
            .unwrap_or(false))
    }

    fn merge_scraper_metadata_into_candidate(
        candidate_json: &str,
        details: &ScraperMetadata,
    ) -> Result<String, MetadataCandidateError> {
        let mut value = serde_json::from_str::<Value>(candidate_json)
            .map_err(|error| MetadataCandidateError::InvalidCandidateJson(error.to_string()))?;
        let mut provider_ids = candidate_provider_ids(&value)
            .map_err(|error| MetadataCandidateError::InvalidCandidateJson(error.to_string()))?;
        provider_ids.extend(details.provider_ids.clone());
        let object = value.as_object_mut().ok_or_else(|| {
            MetadataCandidateError::InvalidCandidateJson("candidate must be an object".to_owned())
        })?;

        macro_rules! set_optional {
            ($field:ident, $key:literal) => {
                if let Some(value) = details.$field.as_ref() {
                    object.insert($key.to_owned(), json!(value));
                }
            };
        }
        set_optional!(title, "title");
        set_optional!(original_title, "originalTitle");
        set_optional!(overview, "overview");
        set_optional!(tagline, "tagline");
        set_optional!(website, "website");
        set_optional!(production_year, "productionYear");
        set_optional!(rating, "rating");
        set_optional!(votes, "votes");
        set_optional!(runtime, "runtime");
        set_optional!(premiere_date, "premiereDate");
        set_optional!(original_language, "originalLanguage");
        set_optional!(end_date, "endDate");
        set_optional!(status, "status");
        set_optional!(set_name, "setName");
        set_optional!(set_id, "setId");
        set_optional!(poster_url, "posterUrl");
        set_optional!(backdrop_url, "backdropUrl");
        set_optional!(certification, "certification");
        if !details.genres.is_empty() {
            object.insert("genres".to_owned(), json!(details.genres));
        }
        if !details.countries.is_empty() {
            object.insert("countries".to_owned(), json!(details.countries));
        }
        if !details.studios.is_empty() {
            object.insert("studios".to_owned(), json!(details.studios));
        }
        object.insert("providerIds".to_owned(), json!(provider_ids));
        object.insert("metadataFetched".to_owned(), Value::Bool(true));
        serde_json::to_string(&value)
            .map_err(|error| MetadataCandidateError::InvalidCandidateJson(error.to_string()))
    }

    async fn search_child_and_store(
        &self,
        item_id: &str,
        query: &str,
        year: Option<i32>,
        current: &StoredMediaMetadata,
        scraper: &ScraperProvider,
        plan: MetadataRequestPlan,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        let item_type = match current.item_type.as_str() {
            "SEASON" => ScraperItemType::Season,
            "EPISODE" => ScraperItemType::Episode,
            _ => return Err(MetadataCandidateError::InvalidSearch),
        };
        let season_number = current
            .season_number
            .and_then(|value| i32::try_from(value).ok())
            .filter(|value| (-1..=1000).contains(value))
            .ok_or(MetadataCandidateError::InvalidSearch)?;
        let episode_number = match item_type {
            ScraperItemType::Episode => Some(
                current
                    .episode_number
                    .and_then(|value| i32::try_from(value).ok())
                    .filter(|value| (0..=10000).contains(value))
                    .ok_or(MetadataCandidateError::InvalidSearch)?,
            ),
            ScraperItemType::Season => None,
            _ => None,
        };
        let raw_series_query = current
            .series_title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(query.trim());
        let parsed = parse_media_name(raw_series_query, MediaKind::Series);
        let series_query = parsed
            .as_ref()
            .map(|value| value.title.as_str())
            .unwrap_or_else(|| raw_series_query.trim());
        let series_year = year.or_else(|| {
            current
                .series_production_year
                .and_then(|value| i32::try_from(value).ok())
        });
        if series_query.is_empty() || series_query.chars().count() > 128 {
            return Err(MetadataCandidateError::InvalidSearch);
        }
        if series_year.is_some_and(|value| !(1800..=2200).contains(&value)) {
            return Err(MetadataCandidateError::InvalidSearch);
        }

        let parents = self
            .parent_providers(current, scraper, series_query, series_year)
            .await?;
        if parents.is_empty() {
            return Err(MetadataCandidateError::Scraper(ScraperError::Provider(
                "series scraper returned no candidates".to_owned(),
            )));
        }
        let expires_at = candidate_expiry();
        let mut stored = false;
        let mut last_error = None;
        for parent in parents {
            let request = match item_type {
                ScraperItemType::Season => {
                    ScraperGetRequest::for_season(&parent.provider_id, season_number, "zh-CN")
                }
                ScraperItemType::Episode => ScraperGetRequest::for_episode(
                    &parent.provider_id,
                    season_number,
                    episode_number.unwrap_or_default(),
                    "zh-CN",
                ),
                _ => continue,
            };
            let selected_child_provider_id = selected_scraper_provider_id(current, scraper);
            let metadata = if plan.needs_metadata || selected_child_provider_id.is_none() {
                match scraper.get_generic(request).await {
                    Ok(metadata) => metadata,
                    Err(error) => {
                        last_error = Some(error.to_string());
                        continue;
                    }
                }
            } else {
                ScraperMetadata {
                    item_type: Some(item_type.as_str().to_owned()),
                    title: Some(current.title.clone()),
                    original_title: current.original_title.clone(),
                    overview: current.overview.clone(),
                    production_year: current
                        .production_year
                        .and_then(|value| i32::try_from(value).ok()),
                    premiere_date: current.premiere_date.clone(),
                    original_language: current.original_language.clone(),
                    provider_ids: current_provider_ids(current),
                    ..ScraperMetadata::default()
                }
            };
            let metadata_fetched = plan.needs_metadata || selected_child_provider_id.is_none();
            let Some(provider_id) = selected_child_provider_id
                .or_else(|| selected_metadata_provider_id(&metadata, &parent.provider))
            else {
                continue;
            };
            let images_response = if plan.needs_images {
                let mut image_request =
                    ScraperImageRequest::new(item_type, &parent.provider_id, "zh-CN");
                image_request.season_number = Some(season_number);
                image_request.episode_number = episode_number;
                scraper.images_generic(image_request).await.ok()
            } else {
                None
            };
            if images_response
                .as_ref()
                .is_some_and(|response| response.images.is_empty())
            {
                self.record_explicitly_unavailable_images(item_id, scraper, &parent.provider_id)
                    .await?;
            }
            let images = images_response.unwrap_or_default();
            let title = metadata
                .title
                .clone()
                .or_else(|| metadata.original_title.clone())
                .unwrap_or_else(|| current.title.clone());
            let mut provider_ids = metadata.provider_ids;
            provider_ids
                .entry(parent.provider.clone())
                .or_insert_with(|| parent.provider_id.clone());
            self.store_candidate(
                item_id,
                current,
                CandidateMetadata {
                    title,
                    original_title: metadata.original_title,
                    overview: metadata.overview,
                    release_date: metadata.premiere_date,
                    end_date: metadata.end_date,
                    status: metadata.status,
                    production_year: metadata.production_year,
                    rating: metadata.rating,
                    original_language: metadata.original_language,
                    tagline: metadata.tagline,
                    website: metadata.website,
                    set_name: metadata.set_name,
                    set_id: metadata.set_id,
                    poster_url: metadata.poster_url,
                    backdrop_url: metadata.backdrop_url,
                    runtime: metadata.runtime,
                    votes: metadata.votes,
                    certification: metadata.certification,
                    countries: metadata.countries,
                    genres: metadata.genres,
                    studios: metadata.studios,
                    provider_ids,
                    directors: Vec::new(),
                    writers: Vec::new(),
                    trailers: Vec::new(),
                    provider: parent.provider,
                    provider_id,
                    images: generic_candidate_images(&images.images, item_type),
                    actors: Vec::new(),
                    metadata_fetched,
                    score: Some(parent.score),
                },
                expires_at,
            )
            .await?;
            stored = true;
        }
        if !stored {
            return Err(MetadataCandidateError::Scraper(ScraperError::Provider(
                last_error.unwrap_or_else(|| "scraper returned no child metadata".to_owned()),
            )));
        }
        self.list_for_item(item_id, None, 0, 50).await
    }

    async fn parent_providers(
        &self,
        current: &StoredMediaMetadata,
        scraper: &ScraperProvider,
        series_query: &str,
        series_year: Option<i32>,
    ) -> Result<Vec<ParentProvider>, MetadataCandidateError> {
        if let (Some(provider), Some(provider_id)) = (
            current.series_provider_name.as_deref(),
            current.series_provider_id.as_deref(),
        ) {
            return Ok(vec![ParentProvider {
                provider: provider.to_owned(),
                provider_id: provider_id.to_owned(),
                score: 100.0,
            }]);
        }
        if let Some(series_item_id) = current.series_item_id.as_deref() {
            let candidates = self
                .database
                .list_pending_metadata_candidates_for_item(series_item_id, None, 0, 20)
                .await?;
            // Keep each child bounded to one parent identity; alternatives remain on the series.
            if let Some(candidate) = candidates
                .into_iter()
                .max_by(|left, right| left.score.total_cmp(&right.score))
            {
                return Ok(vec![ParentProvider {
                    provider: candidate.provider,
                    provider_id: candidate.provider_id,
                    score: candidate.score,
                }]);
            }
        }
        let response = search_generic(scraper, ScraperItemType::Series, series_query, series_year)
            .await
            .map_err(MetadataCandidateError::Scraper)?;
        let best = response
            .items
            .into_iter()
            .filter_map(|result| {
                let (provider, provider_id) = scraper.selected_provider_entry(&result)?;
                Some(ParentProvider {
                    provider: provider.to_owned(),
                    provider_id: provider_id.to_owned(),
                    score: metadata_match_score(
                        current.series_title.as_deref().unwrap_or(series_query),
                        current.series_production_year,
                        result.title.as_deref(),
                        result.original_title.as_deref(),
                        result.production_year,
                    ),
                })
            })
            .max_by(|left, right| left.score.total_cmp(&right.score));
        Ok(best.into_iter().collect())
    }

    async fn store_candidate(
        &self,
        item_id: &str,
        current: &StoredMediaMetadata,
        candidate: CandidateMetadata,
        expires_at: Option<i64>,
    ) -> Result<(), MetadataCandidateError> {
        let score = candidate.score.unwrap_or_else(|| {
            metadata_match_score(
                &current.title,
                current.production_year,
                Some(&candidate.title),
                candidate.original_title.as_deref(),
                candidate.production_year,
            )
        });
        let candidate_json = json!({
            "title": candidate.title,
            "originalTitle": candidate.original_title,
            "overview": candidate.overview,
            "tagline": candidate.tagline,
            "website": candidate.website,
            "releaseDate": candidate.release_date,
            "premiereDate": candidate.release_date,
            "endDate": candidate.end_date,
            "status": candidate.status,
            "setName": candidate.set_name,
            "setId": candidate.set_id,
            "posterUrl": candidate.poster_url,
            "backdropUrl": candidate.backdrop_url,
            "productionYear": candidate.production_year,
            "rating": candidate.rating,
            "votes": candidate.votes,
            "runtime": candidate.runtime,
            "certification": candidate.certification,
            "countries": candidate.countries,
            "genres": candidate.genres,
            "studios": candidate.studios,
            "providerIds": candidate.provider_ids,
            "directors": candidate.directors,
            "writers": candidate.writers,
            "trailers": candidate.trailers,
            "originalLanguage": candidate.original_language,
            "images": candidate.images,
            "actors": candidate.actors,
            "metadataFetched": candidate.metadata_fetched,
        })
        .to_string();
        let id = uuid::Uuid::now_v7().to_string();
        let provider_id = candidate.provider_id.as_str();
        self.database
            .insert_metadata_candidate(NewMetadataCandidate {
                id: &id,
                item_id,
                provider: &candidate.provider,
                provider_id,
                candidate_json: &candidate_json,
                score,
                expires_at,
            })
            .await
            .map_err(MetadataCandidateError::Storage)
    }

    async fn record_explicitly_unavailable_images(
        &self,
        item_id: &str,
        scraper: &ScraperProvider,
        provider_id: &str,
    ) -> Result<(), MetadataCandidateError> {
        let sources = [
            scraper.plugin_id().unwrap_or(scraper.provider_key()),
            scraper.provider_key(),
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .unwrap_or_default();
        for source in sources {
            for image_type in SCRAPER_IMAGE_TYPES {
                let candidate_key = image_no_candidate_key(&source, image_type, provider_id);
                self.database
                    .mark_metadata_image_unavailable(item_id, image_type, &candidate_key, now)
                    .await
                    .map_err(MetadataCandidateError::Storage)?;
            }
        }
        Ok(())
    }
}

struct CandidateMetadata {
    title: String,
    original_title: Option<String>,
    overview: Option<String>,
    tagline: Option<String>,
    website: Option<String>,
    release_date: Option<String>,
    end_date: Option<String>,
    status: Option<String>,
    set_name: Option<String>,
    set_id: Option<String>,
    poster_url: Option<String>,
    backdrop_url: Option<String>,
    production_year: Option<i32>,
    rating: Option<f64>,
    original_language: Option<String>,
    runtime: Option<i32>,
    votes: Option<i64>,
    certification: Option<String>,
    countries: Vec<String>,
    genres: Vec<String>,
    studios: Vec<String>,
    provider_ids: BTreeMap<String, String>,
    directors: Vec<MovieNfoCredit>,
    writers: Vec<MovieNfoCredit>,
    trailers: Vec<String>,
    provider: String,
    provider_id: String,
    images: BTreeMap<String, Vec<String>>,
    actors: Vec<ActorCredit>,
    metadata_fetched: bool,
    score: Option<f64>,
}

#[derive(Clone, Copy)]
enum CandidateSearchMode {
    Manual,
    AutomaticReuse,
    AutomaticFresh,
}

struct ParentProvider {
    provider: String,
    provider_id: String,
    score: f64,
}

async fn search_generic(
    scraper: &ScraperProvider,
    item_type: crate::application::scraper::ScraperItemType,
    query: &str,
    year: Option<i32>,
) -> Result<
    crate::application::scraper::ScraperSearchResponse,
    crate::application::scraper::ScraperError,
> {
    let terms = title_candidates(query);
    let years = match year {
        Some(year) => vec![Some(year), None],
        None => vec![None],
    };
    for search_year in years {
        for term in &terms {
            let response = scraper
                .search_generic(crate::application::scraper::ScraperSearchRequest::new(
                    item_type,
                    term,
                    search_year,
                    "zh-CN",
                ))
                .await?;
            if !response.items.is_empty() {
                return Ok(response);
            }
        }
    }
    Err(crate::application::scraper::ScraperError::Provider(
        "scraper returned no candidates".to_owned(),
    ))
}

fn candidate_expiry() -> Option<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .and_then(|now| now.checked_add(24 * 60 * 60))
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

fn capability_error_is_permanent(error: &ScraperError) -> bool {
    match error {
        ScraperError::UnsupportedCapability(_) => true,
        ScraperError::Provider(message) => {
            let message = message.to_ascii_lowercase();
            message.contains("not found") || message.contains("404")
        }
        _ => false,
    }
}

fn selected_metadata_provider_id(metadata: &ScraperMetadata, provider: &str) -> Option<String> {
    let short_provider = provider.rsplit(['.', ':', '/']).next().unwrap_or(provider);
    metadata
        .provider_id(provider)
        .or_else(|| {
            (short_provider != provider)
                .then(|| metadata.provider_id(short_provider))
                .flatten()
        })
        .map(str::to_owned)
}

fn current_provider_ids(current: &StoredMediaMetadata) -> BTreeMap<String, String> {
    current
        .provider_ids_json
        .as_deref()
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or_default()
}

fn image_attempt_identities(current: &StoredMediaMetadata) -> Vec<(String, String)> {
    let provider_ids = current_provider_ids(current);
    let Some(source) = current
        .metadata_scraper_id
        .as_deref()
        .or(current.scraper_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Vec::new();
    };
    let Some(provider_id) = provider_id_for_key(&provider_ids, source)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Vec::new();
    };
    vec![(provider_key_from_plugin_id(source), provider_id.to_owned())]
}

fn selected_capability_identity(current: &StoredMediaMetadata) -> Option<(String, String)> {
    let provider = current
        .metadata_scraper_id
        .as_deref()
        .or(current.scraper_id.as_deref())?
        .trim();
    let provider_ids = current_provider_ids(current);
    let provider_id = provider_id_for_key(&provider_ids, provider)?.trim();
    (!provider_id.is_empty()).then(|| (provider.to_owned(), provider_id.to_owned()))
}

fn capability_needs_request(
    states: &[StoredMetadataCapabilityAttempt],
    identity: Option<&(String, String)>,
    capability: &str,
) -> bool {
    let Some((provider, provider_id)) = identity else {
        return true;
    };
    let Some(state) = states.iter().find(|state| {
        state.capability.eq_ignore_ascii_case(capability)
            && state.provider_id == *provider_id
            && provider_key_from_plugin_id(&state.provider) == provider_key_from_plugin_id(provider)
    }) else {
        return true;
    };
    match state.status.as_str() {
        "AVAILABLE" | "UNAVAILABLE" => false,
        "FAILED" => state
            .next_retry_at
            .is_none_or(|retry_at| retry_at <= current_unix_timestamp()),
        _ => true,
    }
}

fn selected_scraper_provider_id(
    current: &StoredMediaMetadata,
    scraper: &ScraperProvider,
) -> Option<String> {
    let selected_scraper = current
        .metadata_scraper_id
        .as_deref()
        .or(current.scraper_id.as_deref())?
        .trim();
    if !scraper.matches_scraper_id(selected_scraper) {
        return None;
    }
    let raw = current.provider_ids_json.as_deref()?;
    let provider_ids = serde_json::from_str::<BTreeMap<String, String>>(raw).ok()?;
    provider_id_for_key(&provider_ids, scraper.provider_key())
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && value.chars().count() <= 128
                && !value.chars().any(char::is_control)
        })
        .map(str::to_owned)
}

fn metadata_match_score(
    current_title: &str,
    current_year: Option<i64>,
    candidate_title: Option<&str>,
    candidate_original_title: Option<&str>,
    candidate_year: Option<i32>,
) -> f64 {
    let current_normalized = crate::application::media_matching::normalize_title(current_title);
    let title_score = [candidate_title, candidate_original_title]
        .into_iter()
        .flatten()
        .map(crate::application::media_matching::normalize_title)
        .filter(|title| !title.is_empty())
        .map(|candidate_normalized| {
            title_similarity_score(&current_normalized, &candidate_normalized)
        })
        .max_by(f64::total_cmp)
        .unwrap_or(0.0);
    if title_score == 0.0 {
        return 0.0;
    }

    let year_score = match (
        current_year.and_then(|value| i32::try_from(value).ok()),
        candidate_year,
    ) {
        (Some(current), Some(candidate)) => match current.abs_diff(candidate) {
            0 => 30.0,
            1 => 20.0,
            2..=3 => 5.0,
            _ => -20.0,
        },
        _ => 0.0,
    };
    (title_score + year_score).max(0.0)
}

fn search_result_score(current: &StoredMediaMetadata, result: &ScraperSearchResult) -> f64 {
    metadata_match_score(
        &current.title,
        current.production_year,
        result.title.as_deref(),
        result.original_title.as_deref(),
        result.production_year,
    )
}

fn title_similarity_score(current: &str, candidate: &str) -> f64 {
    if current == candidate {
        return 65.0;
    }
    if candidate.contains(current) || current.contains(candidate) {
        return 45.0;
    }
    match similarity_percent(current, candidate) {
        90..=100 => 50.0,
        80..=89 => 35.0,
        _ => 0.0,
    }
}

fn similarity_percent(left: &str, right: &str) -> u8 {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let max_length = left.len().max(right.len());
    if max_length == 0 {
        return 100;
    }
    let distance = levenshtein(&left, &right);
    let similarity = 100_usize.saturating_sub(distance.saturating_mul(100) / max_length);
    u8::try_from(similarity).unwrap_or(0)
}

fn levenshtein(left: &[char], right: &[char]) -> usize {
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (left_index, left_character) in left.iter().enumerate() {
        let mut current = vec![left_index + 1; right.len() + 1];
        for (right_index, right_character) in right.iter().enumerate() {
            current[right_index + 1] = if left_character == right_character {
                previous[right_index]
            } else {
                1 + previous[right_index]
                    .min(previous[right_index + 1])
                    .min(current[right_index])
            };
        }
        previous = current;
    }
    previous[right.len()]
}

#[derive(Clone, Debug, PartialEq)]
pub struct MetadataCandidatePage {
    pub items: Vec<MetadataCandidateView>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MetadataCandidateView {
    pub id: String,
    pub item_id: String,
    pub item_title: String,
    pub provider: String,
    pub provider_id: String,
    pub candidate: Value,
    pub score: f64,
    pub status: String,
    pub expires_at: Option<i64>,
    pub field_diffs: Vec<MetadataFieldDiff>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MetadataFieldDiff {
    pub field: String,
    pub current: Value,
    pub candidate: Value,
    pub provenance: Option<String>,
}

#[derive(Debug)]
pub enum MetadataCandidateError {
    ItemNotFound,
    InvalidSearch,
    InvalidCandidateJson(String),
    Scraper(crate::application::scraper::ScraperError),
    Storage(StorageError),
}

impl fmt::Display for MetadataCandidateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ItemNotFound => formatter.write_str("media item not found"),
            Self::InvalidSearch => formatter.write_str("candidate search is too long"),
            Self::InvalidCandidateJson(error) => {
                write!(formatter, "candidate JSON is invalid: {error}")
            }
            Self::Scraper(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MetadataCandidateError {}

impl From<StorageError> for MetadataCandidateError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

fn generic_candidate_images(
    images: &[crate::application::scraper::ScraperImage],
    item_type: ScraperItemType,
) -> BTreeMap<String, Vec<String>> {
    let mut result = BTreeMap::<String, Vec<String>>::new();
    for image in images {
        let image_types: &[&str] = match image.image_type.as_str() {
            "Primary" | "Poster" | "POSTER" => &["POSTER", "DISC"],
            "Logo" | "LOGO" => &["LOGO"],
            "Backdrop" | "Fanart" | "FANART" if item_type == ScraperItemType::Episode => {
                &["FANART"]
            }
            "Backdrop" | "Fanart" | "FANART" => &["FANART", "THUMB", "BANNER", "ART", "WALLPAPER"],
            "Thumb" | "THUMB" if item_type == ScraperItemType::Episode => &["FANART"],
            "Thumb" | "THUMB" => &["THUMB"],
            "Banner" | "BANNER" => &["BANNER"],
            "Disc" | "DISC" => &["DISC"],
            "Art" | "ART" => &["ART"],
            "Wallpaper" | "WALLPAPER" => &["WALLPAPER"],
            _ => continue,
        };
        for image_type in image_types {
            result
                .entry((*image_type).to_owned())
                .or_default()
                .push(image.url.clone());
        }
    }
    result
}

fn generic_candidate_actors(
    cast: &[crate::application::scraper::ScraperActorCredit],
) -> Vec<ActorCredit> {
    cast.iter()
        .take(MAX_MOVIE_NFO_ACTORS)
        .filter_map(|member| {
            let id = member.provider_id.trim();
            if !id.is_empty() && !valid_person_id(id) {
                return None;
            }
            let name = member.name.as_deref()?.trim();
            (!name.is_empty()).then(|| ActorCredit {
                id: id.to_owned(),
                provider: None,
                identities: Vec::new(),
                name: name.to_owned(),
                character: member
                    .character
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
                order: member.order,
                profile_url: member.profile_url.clone(),
                person: None,
            })
        })
        .collect()
}

async fn enrich_actor_metadata(scraper: &ScraperProvider, actors: &mut [ActorCredit]) {
    let requests = actors
        .iter()
        .enumerate()
        .filter_map(|(index, actor)| {
            let provider_id = actor.id.trim();
            (!provider_id.is_empty()).then(|| (index, provider_id.to_owned()))
        })
        .collect::<Vec<_>>();
    let mut next_request = 0;
    let mut pending = JoinSet::new();
    while next_request < requests.len() || !pending.is_empty() {
        while next_request < requests.len() && pending.len() < ACTOR_METADATA_FETCH_CONCURRENCY {
            let (index, provider_id) = &requests[next_request];
            let index = *index;
            let scraper = scraper.clone();
            let provider_id = provider_id.clone();
            pending.spawn(async move {
                let request = ScraperGetRequest::new(ScraperItemType::Person, provider_id, "zh-CN");
                (index, scraper.get_generic(request).await.ok())
            });
            next_request += 1;
        }
        let Some(result) = pending.join_next().await else {
            break;
        };
        let Ok((index, Some(metadata))) = result else {
            continue;
        };
        let person = crate::application::people::PersonMetadata {
            biography: metadata.overview,
            birthday: metadata.birthday,
            deathday: metadata.deathday,
            known_for_department: metadata.known_for_department,
            place_of_birth: metadata.place_of_birth,
            provider_ids: std::collections::BTreeMap::new(),
            genres: Vec::new(),
            tags: Vec::new(),
            production_locations: Vec::new(),
            premiere_date: None,
            production_year: None,
            taglines: Vec::new(),
        };
        if person.biography.is_some()
            || person.birthday.is_some()
            || person.deathday.is_some()
            || person.known_for_department.is_some()
            || person.place_of_birth.is_some()
        {
            actors[index].person = Some(person);
        }
    }
}

#[derive(Clone, Copy)]
enum CrewRole {
    Director,
    Writer,
}

fn generic_candidate_crew(
    crew: &[crate::application::scraper::ScraperCrewCredit],
    role: CrewRole,
) -> Vec<MovieNfoCredit> {
    crew.iter()
        .filter(|credit| {
            let department = credit.department.as_deref().unwrap_or_default();
            let job = credit.job.as_deref().unwrap_or_default();
            match role {
                CrewRole::Director => {
                    department.eq_ignore_ascii_case("Directing")
                        && job.eq_ignore_ascii_case("Director")
                }
                CrewRole::Writer => {
                    department.eq_ignore_ascii_case("Writing")
                        && matches!(
                            job.to_ascii_lowercase().as_str(),
                            "writer" | "screenplay" | "story" | "author"
                        )
                }
            }
        })
        .filter_map(|credit| {
            let id = credit.provider_id.trim();
            let name = credit.name.as_deref()?.trim();
            (valid_person_id(id) && !name.is_empty()).then(|| MovieNfoCredit {
                provider_id: id.to_owned(),
                name: name.to_owned(),
            })
        })
        .collect()
}

fn valid_person_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MetadataSelectionMode {
    FillMissing,
    RefreshUnlocked,
}

impl MetadataSelectionMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FillMissing => "fillMissing",
            Self::RefreshUnlocked => "refreshUnlocked",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataSelectionReport {
    pub item_id: String,
    pub candidate_id: String,
    pub mode: MetadataSelectionMode,
    pub status: &'static str,
    pub image_types: Vec<&'static str>,
    pub actor_count: usize,
}

struct MetadataSelectionOptions<'a> {
    keep_pending: bool,
    scraper_id: Option<&'a str>,
    supplemental: bool,
    image_policy: Option<ImageSelectionPolicy>,
}

#[derive(Clone)]
pub struct MetadataSelectionService {
    database: Database,
    nfo: NfoWriteService,
    images: ImageWriteService,
    people: crate::application::people::PeopleService,
    resources: ResourceMetrics,
    home: Option<HomeService>,
}

impl MetadataSelectionService {
    pub fn new(database: Database, images: ImageWriteService) -> Self {
        Self::with_config_dir(database, images, std::path::PathBuf::from("./config"))
    }

    pub fn with_config_dir(
        database: Database,
        images: ImageWriteService,
        config_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            nfo: NfoWriteService::new(database.clone()),
            database: database.clone(),
            images,
            people: crate::application::people::PeopleService::new(config_dir)
                .with_database(database.clone()),
            resources: ResourceMetrics::new(),
            home: None,
        }
    }

    pub(crate) fn with_home(mut self, home: HomeService) -> Self {
        self.home = Some(home);
        self
    }

    pub(crate) fn with_resource_metrics(mut self, resources: ResourceMetrics) -> Self {
        self.images = self.images.clone().with_resource_metrics(resources.clone());
        self.resources = resources;
        self
    }

    pub(crate) async fn fill_missing_request_plan(
        &self,
        item_id: &str,
    ) -> Result<MetadataRequestPlan, MetadataSelectionError> {
        let current = self
            .database
            .find_media_item_metadata(item_id)
            .await?
            .ok_or(MetadataSelectionError::ItemNotFound)?;
        self.fill_missing_request_plan_for_current(item_id, &current)
            .await
    }

    pub(crate) async fn fill_missing_request_plan_for_current(
        &self,
        item_id: &str,
        current: &StoredMediaMetadata,
    ) -> Result<MetadataRequestPlan, MetadataSelectionError> {
        if fill_missing_fields(&current.item_type).is_none() {
            return Ok(MetadataRequestPlan::full());
        }
        let image_policy = self.image_selection_policy(item_id).await?;
        let capability_states = self
            .database
            .list_metadata_capability_attempts(item_id)
            .await?;
        let image_attempts = self.database.list_metadata_image_attempts(item_id).await?;
        let unavailable_image_attempts = image_attempts
            .into_iter()
            .filter(|attempt| attempt.status.eq_ignore_ascii_case("UNAVAILABLE"))
            .map(|attempt| (attempt.image_type, attempt.candidate_key))
            .collect::<BTreeSet<_>>();
        let image_attempt_identities = image_attempt_identities(current);
        let image_types = image_policy.enabled_types().collect::<Vec<_>>();
        let local_image_types = self.images.local_image_types(item_id, &image_types).await?;
        let mut images_missing = false;
        for image_type in image_types {
            if local_image_types.contains(image_type) {
                continue;
            }
            let explicitly_unavailable = !image_attempt_identities.is_empty()
                && image_attempt_identities
                    .iter()
                    .all(|(source, provider_id)| {
                        unavailable_image_attempts.contains(&(
                            image_type.to_owned(),
                            image_no_candidate_key(source, image_type, provider_id),
                        ))
                    });
            if !explicitly_unavailable {
                images_missing = true;
                break;
            }
        }
        let details = current.nfo_metadata_json.as_deref().and_then(|value| {
            serde_json::from_str::<crate::application::nfo::LocalNfoDetails>(value).ok()
        });
        let details = if details.is_some() {
            details
        } else {
            self.nfo
                .read_item_projection(item_id)
                .await?
                .map(|projection| projection.details)
        };
        let credits_missing = match current.item_type.as_str() {
            "MOVIE" | "SERIES" => {
                let actor_relation_exists = self
                    .people
                    .item_actor_relation_exists(item_id)
                    .await
                    .map_err(MetadataSelectionError::People)?;
                credits_are_missing(actor_relation_exists, details.as_ref())
            }
            _ => false,
        };
        let mut plan =
            metadata_request_plan(current, images_missing, credits_missing, details.as_ref());
        plan.image_policy = Some(image_policy);
        let capability_identity = selected_capability_identity(current);
        plan.needs_credits = plan.needs_credits
            && capability_needs_request(
                &capability_states,
                capability_identity.as_ref(),
                CAPABILITY_CREDITS,
            );
        plan.needs_external_ids = plan.needs_external_ids
            && capability_needs_request(
                &capability_states,
                capability_identity.as_ref(),
                CAPABILITY_EXTERNAL_IDS,
            );
        plan.needs_trailers = plan.needs_trailers
            && capability_needs_request(
                &capability_states,
                capability_identity.as_ref(),
                CAPABILITY_TRAILERS,
            );
        if !has_selected_provider_id(current) {
            plan.needs_metadata = true;
        }
        Ok(plan)
    }

    pub async fn select(
        &self,
        item_id: &str,
        candidate_id: &str,
        mode: MetadataSelectionMode,
    ) -> Result<MetadataSelectionReport, MetadataSelectionError> {
        self.select_internal(
            item_id,
            candidate_id,
            mode,
            MetadataSelectionOptions {
                keep_pending: false,
                scraper_id: None,
                supplemental: false,
                image_policy: None,
            },
        )
        .await
    }

    pub async fn select_with_scraper(
        &self,
        item_id: &str,
        candidate_id: &str,
        mode: MetadataSelectionMode,
        scraper_id: Option<&str>,
        supplemental: bool,
    ) -> Result<MetadataSelectionReport, MetadataSelectionError> {
        self.select_with_scraper_and_policy(
            item_id,
            candidate_id,
            mode,
            scraper_id,
            supplemental,
            None,
        )
        .await
    }

    pub(crate) async fn select_with_scraper_and_policy(
        &self,
        item_id: &str,
        candidate_id: &str,
        mode: MetadataSelectionMode,
        scraper_id: Option<&str>,
        supplemental: bool,
        image_policy: Option<ImageSelectionPolicy>,
    ) -> Result<MetadataSelectionReport, MetadataSelectionError> {
        self.select_internal(
            item_id,
            candidate_id,
            mode,
            MetadataSelectionOptions {
                keep_pending: false,
                scraper_id,
                supplemental,
                image_policy,
            },
        )
        .await
    }

    pub(crate) async fn select_for_review_with_scraper_and_policy(
        &self,
        item_id: &str,
        candidate_id: &str,
        mode: MetadataSelectionMode,
        scraper_id: Option<&str>,
        supplemental: bool,
        image_policy: Option<ImageSelectionPolicy>,
    ) -> Result<MetadataSelectionReport, MetadataSelectionError> {
        self.select_internal(
            item_id,
            candidate_id,
            mode,
            MetadataSelectionOptions {
                keep_pending: true,
                scraper_id,
                supplemental,
                image_policy,
            },
        )
        .await
    }

    pub async fn select_for_review(
        &self,
        item_id: &str,
        candidate_id: &str,
        mode: MetadataSelectionMode,
    ) -> Result<MetadataSelectionReport, MetadataSelectionError> {
        self.select_internal(
            item_id,
            candidate_id,
            mode,
            MetadataSelectionOptions {
                keep_pending: true,
                scraper_id: None,
                supplemental: false,
                image_policy: None,
            },
        )
        .await
    }

    pub(crate) async fn enrich_selected_actors(
        &self,
        item_id: &str,
        candidate_id: &str,
        scraper: &ScraperProvider,
    ) -> Result<usize, MetadataSelectionError> {
        let candidate = self
            .database
            .find_metadata_candidate(item_id, candidate_id)
            .await?
            .ok_or(MetadataSelectionError::CandidateNotFound)?;
        let mut actors = candidate_payload(&candidate)?.actors;
        if actors.is_empty() {
            return Ok(0);
        }
        enrich_actor_metadata(scraper, &mut actors).await;
        for actor in &mut actors {
            if actor.provider.is_none() && !actor.id.trim().is_empty() {
                actor.provider = Some(candidate.provider.to_ascii_lowercase());
            }
        }
        self.people
            .update_item_actor_metadata(item_id, &candidate.provider, &actors)
            .await
            .map_err(MetadataSelectionError::People)
    }

    pub async fn confirm_best_pending(
        &self,
        item_id: &str,
    ) -> Result<MetadataSelectionReport, MetadataSelectionError> {
        let candidate = self
            .database
            .find_best_pending_metadata_candidate(item_id)
            .await?
            .ok_or(MetadataSelectionError::CandidateNotFound)?;
        self.select(item_id, &candidate.id, MetadataSelectionMode::FillMissing)
            .await
    }

    async fn select_internal(
        &self,
        item_id: &str,
        candidate_id: &str,
        mode: MetadataSelectionMode,
        options: MetadataSelectionOptions<'_>,
    ) -> Result<MetadataSelectionReport, MetadataSelectionError> {
        let current = self
            .database
            .find_media_item_metadata(item_id)
            .await?
            .ok_or(MetadataSelectionError::ItemNotFound)?;
        let candidate = self
            .database
            .find_metadata_candidate(item_id, candidate_id)
            .await?
            .ok_or(MetadataSelectionError::CandidateNotFound)?;
        if candidate.status != "PENDING" {
            return Err(MetadataSelectionError::CandidateNotPending(
                candidate.status,
            ));
        }
        let mut payload = candidate_payload(&candidate)?;
        let candidate_provider = candidate.provider.trim().to_ascii_lowercase();
        for actor in &mut payload.actors {
            if actor.provider.is_none() && !actor.id.trim().is_empty() {
                actor.provider = Some(candidate_provider.clone());
            }
        }
        payload.movie_nfo.actors = payload.actors.clone();
        if matches!(mode, MetadataSelectionMode::FillMissing) || options.supplemental {
            let projection = self.nfo.read_item_projection(item_id).await?;
            merge_supplemental_movie_nfo(
                &mut payload.movie_nfo,
                projection.as_ref(),
                options.supplemental,
            );
            payload.actors = payload.movie_nfo.actors.clone();
            if options.supplemental {
                preserve_supplemental_scalar_values(&mut payload, &current);
            }
        }
        let image_source = options.scraper_id.unwrap_or(candidate.provider.as_str());
        let image_policy = match options.image_policy {
            Some(image_policy) => image_policy,
            None => self.image_selection_policy(item_id).await?,
        };
        let mut state = metadata_state(&current);
        let metadata_candidate = MetadataCandidate {
            source: MetadataSource::ScraperLocalized,
            metadata: payload.metadata.clone(),
        };
        match mode {
            MetadataSelectionMode::FillMissing => state.apply_fill_missing(&metadata_candidate),
            MetadataSelectionMode::RefreshUnlocked => {
                state.apply_refresh_unlocked(&metadata_candidate)
            }
        }
        let mut movie_nfo = payload.movie_nfo.clone();
        movie_nfo.base = state.metadata.clone();
        let image_types = self
            .write_selected_images(item_id, &payload, image_policy, image_source, mode)
            .await?;
        let has_primary_artwork = image_types
            .iter()
            .any(|image_type| matches!(*image_type, "POSTER" | "THUMB"))
            || self.images.has_local_image(item_id, "POSTER").await?
            || self.images.has_local_image(item_id, "THUMB").await?;
        let actor_count = self
            .people
            .persist_item_actors(item_id, &candidate.provider, &payload.actors)
            .await?;
        let nfo_started = std::time::Instant::now();
        let nfo_report = if current.item_type == "MOVIE" {
            self.nfo.write_item_movie_nfo(item_id, &movie_nfo).await?
        } else {
            self.nfo.write_item_nfo(item_id, &state.metadata).await?
        };
        self.resources
            .record_metadata_stage("nfo_write", nfo_started.elapsed());
        let mut provider_ids = current_provider_ids(&current);
        provider_ids.extend(movie_nfo.provider_ids.clone());
        if options.supplemental {
            if !provider_ids
                .keys()
                .any(|key| key.eq_ignore_ascii_case(&candidate.provider))
            {
                provider_ids.insert(
                    candidate.provider.to_ascii_lowercase(),
                    candidate.provider_id.clone(),
                );
            }
        } else {
            provider_ids.insert(
                candidate.provider.to_ascii_lowercase(),
                candidate.provider_id.clone(),
            );
        }
        let provider_ids_json = serde_json::to_string(&provider_ids)
            .map_err(|error| MetadataSelectionError::InvalidCandidate(error.to_string()))?;
        let selected = self
            .database
            .select_metadata_candidate(SelectedMetadataUpdate {
                item_id,
                candidate_id,
                title: state.metadata.title.as_deref().unwrap_or(&current.title),
                original_title: state.metadata.original_title.as_deref(),
                overview: state.metadata.overview.as_deref(),
                production_year: state.metadata.production_year.map(i64::from),
                premiere_date: payload.premiere_date.as_deref(),
                last_air_date: payload.end_date.as_deref(),
                status: payload.status.as_deref(),
                original_language: payload.original_language.as_deref(),
                rating: payload.rating,
                rating_source: payload.rating.as_ref().map(|_| candidate.provider.as_str()),
                provider_ids_json: &provider_ids_json,
                metadata_scraper_id: (!options.supplemental && !options.keep_pending)
                    .then_some(options.scraper_id)
                    .flatten(),
                metadata_fingerprint: &nfo_report.fingerprint,
                provenance_json: &state.provenance_json(),
                locked_fields_json: &state.locked_fields_json(),
                poster_fallback_required: !has_primary_artwork,
                keep_pending: options.keep_pending,
            })
            .await?;
        if !selected {
            return Err(MetadataSelectionError::CandidateNotPending(
                "CONCURRENTLY_SELECTED".to_owned(),
            ));
        }
        if let Some(home) = &self.home {
            home.invalidate();
        }
        Ok(MetadataSelectionReport {
            item_id: item_id.to_owned(),
            candidate_id: candidate_id.to_owned(),
            mode,
            status: if options.keep_pending {
                "PENDING"
            } else {
                "ONLINE_CONFIRMED"
            },
            image_types,
            actor_count,
        })
    }

    async fn write_selected_images(
        &self,
        item_id: &str,
        payload: &CandidatePayload,
        image_policy: ImageSelectionPolicy,
        source: &str,
        mode: MetadataSelectionMode,
    ) -> Result<Vec<&'static str>, MetadataSelectionError> {
        let specs = if payload.typed_images_present {
            image_policy
                .enabled_types()
                .filter_map(|image_type| {
                    let urls = payload.images.get(image_type)?.clone();
                    (!urls.is_empty()).then_some((image_type, urls))
                })
                .collect::<Vec<_>>()
        } else {
            [
                ("POSTER", payload.poster_url.clone()),
                ("FANART", payload.fanart_url.clone()),
            ]
            .into_iter()
            .filter_map(|(image_type, url)| url.map(|url| (image_type, vec![url])))
            .collect::<Vec<_>>()
        };
        let item_permits = Arc::new(Semaphore::new(IMAGE_ITEM_CONCURRENCY));
        let mut tasks = JoinSet::new();
        for (index, (image_type, urls)) in specs.into_iter().enumerate() {
            let permit = item_permits.clone().acquire_owned().await.map_err(|_| {
                MetadataSelectionError::InvalidCandidate("image semaphore closed".to_owned())
            })?;
            let images = self.images.clone();
            let item_id = item_id.to_owned();
            let source = source.to_owned();
            tasks.spawn(async move {
                let _permit = permit;
                let mut last_error = None;
                for url in urls.into_iter().take(4) {
                    let result = match mode {
                        MetadataSelectionMode::FillMissing => {
                            images
                                .try_download_item_image_if_missing_from_scraper(
                                    &item_id, image_type, &url, &source,
                                )
                                .await
                        }
                        MetadataSelectionMode::RefreshUnlocked => {
                            images
                                .download_item_image_from_scraper(
                                    &item_id, image_type, &url, &source,
                                )
                                .await
                        }
                    };
                    match result {
                        Ok(Some(report)) => {
                            let _ = report;
                            return Ok(Some((index, image_type)));
                        }
                        Ok(None) => continue,
                        Err(error) => {
                            last_error = Some(error);
                        }
                    }
                }
                if last_error.is_some() {
                    tracing::warn!(
                        item_id,
                        image_type,
                        "metadata image candidates were unavailable"
                    );
                }
                Ok(None)
            });
        }
        let mut image_types = Vec::new();
        while let Some(result) = tasks.join_next().await {
            match result.map_err(|_| {
                MetadataSelectionError::InvalidCandidate("image task failed".to_owned())
            })? {
                Ok(Some(image_type)) => image_types.push(image_type),
                Ok(None) => {}
                Err(error) => return Err(MetadataSelectionError::Image(error)),
            }
        }
        image_types.sort_unstable_by_key(|(index, _)| *index);
        Ok(image_types
            .into_iter()
            .map(|(_, image_type)| image_type)
            .collect())
    }

    async fn image_selection_policy(
        &self,
        item_id: &str,
    ) -> Result<ImageSelectionPolicy, MetadataSelectionError> {
        let library_id = self
            .database
            .find_item_library_id(item_id)
            .await?
            .ok_or(MetadataSelectionError::ItemNotFound)?;
        let library = self
            .database
            .find_library(&library_id)
            .await?
            .ok_or(MetadataSelectionError::ItemNotFound)?;
        let global = self.database.media_strategy_settings().await?;
        Ok(ImageSelectionPolicy::from_json(
            library.media_strategy_json.as_deref(),
            global.as_deref(),
        ))
    }
}

fn merge_supplemental_movie_nfo(
    candidate: &mut MovieNfoMetadata,
    existing: Option<&crate::application::nfo::LocalNfoProjection>,
    append_lists: bool,
) {
    let Some(existing) = existing else {
        return;
    };
    let details = &existing.details;
    macro_rules! preserve {
        ($field:ident) => {
            if candidate.$field.is_none() {
                candidate.$field = details.$field.clone();
            }
        };
    }
    if append_lists {
        replace_if_present(&mut candidate.rating, details.rating);
        replace_if_present(&mut candidate.votes, details.votes);
        replace_if_present(&mut candidate.tagline, details.tagline.clone());
        replace_if_present(&mut candidate.premiered, details.premiered.clone());
    } else {
        preserve!(rating);
        preserve!(votes);
        preserve!(tagline);
        preserve!(premiered);
    }
    if candidate.releasedate.is_none() {
        candidate.releasedate = details.release_date.clone();
    }
    if append_lists {
        replace_if_present(&mut candidate.releasedate, details.release_date.clone());
        replace_if_present(&mut candidate.runtime, details.runtime);
        replace_if_present(&mut candidate.status, details.status.clone());
        replace_if_present(
            &mut candidate.original_language,
            details.original_language.clone(),
        );
        replace_if_present(&mut candidate.website, details.website.clone());
        replace_if_present(&mut candidate.set_name, details.set_name.clone());
        replace_if_present(&mut candidate.set_id, details.set_id.clone());
        replace_if_present(&mut candidate.certification, details.certification.clone());
        candidate.countries = merge_string_values(&details.countries, &candidate.countries);
        candidate.genres = merge_string_values(&details.genres, &candidate.genres);
        candidate.studios = merge_string_values(&details.studios, &candidate.studios);
        candidate.directors = merge_credit_values(&details.directors, &candidate.directors);
        candidate.writers = merge_credit_values(&details.writers, &candidate.writers);
        candidate.trailers = merge_string_values(&details.trailers, &candidate.trailers);
        candidate.actors = merge_actor_values(&existing.actors, &candidate.actors);
    } else {
        preserve!(runtime);
        preserve!(status);
        preserve!(original_language);
        preserve!(website);
        preserve!(set_name);
        preserve!(set_id);
        preserve!(certification);
        if !details.countries.is_empty() {
            candidate.countries = details.countries.clone();
        }
        if !details.genres.is_empty() {
            candidate.genres = details.genres.clone();
        }
        if !details.studios.is_empty() {
            candidate.studios = details.studios.clone();
        }
        if !details.directors.is_empty() {
            candidate.directors = details
                .directors
                .iter()
                .map(|credit| MovieNfoCredit {
                    provider_id: credit.provider_id.clone(),
                    name: credit.name.clone(),
                })
                .collect();
        }
        if !details.writers.is_empty() {
            candidate.writers = details
                .writers
                .iter()
                .map(|credit| MovieNfoCredit {
                    provider_id: credit.provider_id.clone(),
                    name: credit.name.clone(),
                })
                .collect();
        }
        if !details.trailers.is_empty() {
            candidate.trailers = details.trailers.clone();
        }
        if !existing.actors.is_empty() {
            candidate.actors = existing.actors.clone();
        }
    }
    for (provider, id) in &details.provider_ids {
        candidate
            .provider_ids
            .entry(provider.clone())
            .or_insert_with(|| id.clone());
    }
}

fn replace_if_present<T>(target: &mut Option<T>, value: Option<T>) {
    if value.is_some() {
        *target = value;
    }
}

fn merge_string_values(existing: &[String], incoming: &[String]) -> Vec<String> {
    let mut merged = Vec::with_capacity(existing.len() + incoming.len());
    for value in existing.iter().chain(incoming) {
        let trimmed = value.trim();
        if trimmed.is_empty()
            || merged
                .iter()
                .any(|stored: &String| stored.eq_ignore_ascii_case(trimmed))
        {
            continue;
        }
        merged.push(trimmed.to_owned());
    }
    merged
}

fn merge_credit_values(
    existing: &[MovieNfoCredit],
    incoming: &[MovieNfoCredit],
) -> Vec<MovieNfoCredit> {
    let mut merged = Vec::with_capacity(existing.len() + incoming.len());
    for credit in existing.iter().chain(incoming) {
        if credit.name.trim().is_empty() {
            continue;
        }
        let duplicate = merged.iter().any(|stored: &MovieNfoCredit| {
            if !credit.provider_id.trim().is_empty() && !stored.provider_id.trim().is_empty() {
                credit.provider_id.eq_ignore_ascii_case(&stored.provider_id)
            } else {
                credit.name.trim().eq_ignore_ascii_case(stored.name.trim())
            }
        });
        if !duplicate {
            merged.push(credit.clone());
        }
    }
    merged
}

fn merge_actor_values(existing: &[ActorCredit], incoming: &[ActorCredit]) -> Vec<ActorCredit> {
    let mut merged = Vec::with_capacity(existing.len() + incoming.len());
    for actor in existing.iter().chain(incoming) {
        if actor.name.trim().is_empty() {
            continue;
        }
        let duplicate = merged.iter().any(|stored: &ActorCredit| {
            if !actor.id.trim().is_empty() && !stored.id.trim().is_empty() {
                actor
                    .provider
                    .as_deref()
                    .unwrap_or_default()
                    .eq_ignore_ascii_case(stored.provider.as_deref().unwrap_or_default())
                    && actor.id.eq_ignore_ascii_case(&stored.id)
            } else {
                actor.name.trim().eq_ignore_ascii_case(stored.name.trim())
                    && actor
                        .character
                        .as_deref()
                        .unwrap_or_default()
                        .eq_ignore_ascii_case(stored.character.as_deref().unwrap_or_default())
            }
        });
        if !duplicate {
            merged.push(actor.clone());
        }
    }
    merged
}

fn preserve_supplemental_scalar_values(
    candidate: &mut CandidatePayload,
    current: &StoredMediaMetadata,
) {
    if current.premiere_date.is_some() {
        candidate.premiere_date = None;
    }
    if current.last_air_date.is_some() {
        candidate.end_date = None;
    }
    if current.status.is_some() {
        candidate.status = None;
    }
    if current.original_language.is_some() {
        candidate.original_language = None;
    }
    if current.rating.is_some() {
        candidate.rating = None;
    }
}

const MOVIE_FILL_MISSING_FIELDS: &[MetadataField] = &[
    MetadataField::Title,
    MetadataField::OriginalTitle,
    MetadataField::Overview,
    MetadataField::ProductionYear,
];
const SERIES_FILL_MISSING_FIELDS: &[MetadataField] = MOVIE_FILL_MISSING_FIELDS;
const CHILD_FILL_MISSING_FIELDS: &[MetadataField] =
    &[MetadataField::Title, MetadataField::Overview];

fn fill_missing_fields(item_type: &str) -> Option<&'static [MetadataField]> {
    match item_type {
        "MOVIE" => Some(MOVIE_FILL_MISSING_FIELDS),
        "SERIES" => Some(SERIES_FILL_MISSING_FIELDS),
        "SEASON" | "EPISODE" => Some(CHILD_FILL_MISSING_FIELDS),
        _ => None,
    }
}

fn metadata_state(current: &StoredMediaMetadata) -> MetadataState {
    MetadataState::from_persisted(
        NfoMetadata {
            title: Some(current.title.clone()),
            original_title: current.original_title.clone(),
            overview: current.overview.clone(),
            production_year: current
                .production_year
                .and_then(|year| i32::try_from(year).ok()),
        },
        current.provenance_json.as_deref(),
        current.locked_fields_json.as_deref(),
    )
}

fn fill_missing_scalar_values_complete(current: &StoredMediaMetadata) -> bool {
    let has_text = |value: Option<&String>| value.is_some_and(|value| !value.trim().is_empty());
    match current.item_type.as_str() {
        "MOVIE" => {
            has_text(current.premiere_date.as_ref())
                && has_text(current.original_language.as_ref())
                && current.rating.is_some()
        }
        "SERIES" => {
            has_text(current.premiere_date.as_ref())
                && has_text(current.last_air_date.as_ref())
                && has_text(current.status.as_ref())
                && has_text(current.original_language.as_ref())
                && current.rating.is_some()
        }
        "SEASON" | "EPISODE" => has_text(current.premiere_date.as_ref()),
        _ => false,
    }
}

fn has_selected_provider_id(current: &StoredMediaMetadata) -> bool {
    let Some(scraper) = current
        .metadata_scraper_id
        .as_deref()
        .or(current.scraper_id.as_deref())
    else {
        return true;
    };
    let Some(raw) = current.provider_ids_json.as_deref() else {
        return false;
    };
    let Ok(Value::Object(provider_ids)) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    let short_scraper = scraper.rsplit(['.', ':', '/']).next().unwrap_or(scraper);
    provider_ids.iter().any(|(provider, id)| {
        id.as_str().is_some_and(|value| !value.trim().is_empty())
            && (provider.eq_ignore_ascii_case(scraper)
                || provider.eq_ignore_ascii_case(short_scraper))
    })
}

#[derive(Debug)]
pub enum MetadataSelectionError {
    ItemNotFound,
    CandidateNotFound,
    CandidateNotPending(String),
    InvalidCandidate(String),
    Nfo(NfoWriteError),
    Image(ImageWriteError),
    People(PeopleError),
    Storage(StorageError),
}

impl fmt::Display for MetadataSelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ItemNotFound => formatter.write_str("media item not found"),
            Self::CandidateNotFound => formatter.write_str("metadata candidate not found"),
            Self::CandidateNotPending(status) => {
                write!(formatter, "metadata candidate is not pending: {status}")
            }
            Self::InvalidCandidate(message) => {
                write!(formatter, "invalid metadata candidate: {message}")
            }
            Self::Nfo(error) => error.fmt(formatter),
            Self::Image(error) => error.fmt(formatter),
            Self::People(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MetadataSelectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Nfo(error) => Some(error),
            Self::Image(error) => Some(error),
            Self::People(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::ItemNotFound
            | Self::CandidateNotFound
            | Self::CandidateNotPending(_)
            | Self::InvalidCandidate(_) => None,
        }
    }
}

impl From<StorageError> for MetadataSelectionError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<NfoWriteError> for MetadataSelectionError {
    fn from(error: NfoWriteError) -> Self {
        Self::Nfo(error)
    }
}

impl From<ImageWriteError> for MetadataSelectionError {
    fn from(error: ImageWriteError) -> Self {
        Self::Image(error)
    }
}

impl From<PeopleError> for MetadataSelectionError {
    fn from(error: PeopleError) -> Self {
        Self::People(error)
    }
}

struct CandidatePayload {
    metadata: NfoMetadata,
    movie_nfo: MovieNfoMetadata,
    premiere_date: Option<String>,
    end_date: Option<String>,
    status: Option<String>,
    original_language: Option<String>,
    rating: Option<f64>,
    images: BTreeMap<String, Vec<String>>,
    typed_images_present: bool,
    poster_url: Option<String>,
    fanart_url: Option<String>,
    actors: Vec<ActorCredit>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ImageSelectionPolicy {
    poster: bool,
    artwork: bool,
    banner: bool,
    logo: bool,
    thumbnail: bool,
    disc: bool,
    wallpaper: bool,
}

impl ImageSelectionPolicy {
    fn from_json(library: Option<&str>, global: Option<&str>) -> Self {
        library
            .and_then(parse_image_selection_policy)
            .or_else(|| global.and_then(parse_image_selection_policy))
            .unwrap_or_else(default_image_selection_policy)
    }

    fn enabled_types(self) -> impl Iterator<Item = &'static str> {
        [
            (self.poster, "POSTER"),
            (true, "FANART"),
            (self.logo, "LOGO"),
            (self.thumbnail, "THUMB"),
            (self.banner, "BANNER"),
            (self.disc, "DISC"),
            (self.artwork, "ART"),
            (self.wallpaper, "WALLPAPER"),
        ]
        .into_iter()
        .filter_map(|(enabled, image_type)| enabled.then_some(image_type))
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredImageStrategy {
    #[serde(default = "default_true")]
    poster: bool,
    #[serde(default)]
    artwork: bool,
    #[serde(default)]
    banner: bool,
    #[serde(default = "default_true")]
    logo: bool,
    #[serde(default = "default_true")]
    thumbnail: bool,
    #[serde(default)]
    disc: bool,
    #[serde(default)]
    wallpaper: bool,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredMediaStrategy {
    #[serde(default)]
    images: StoredImageStrategy,
}

fn default_true() -> bool {
    true
}

fn default_image_selection_policy() -> ImageSelectionPolicy {
    ImageSelectionPolicy {
        poster: true,
        logo: true,
        thumbnail: true,
        ..ImageSelectionPolicy::default()
    }
}

fn parse_image_selection_policy(value: &str) -> Option<ImageSelectionPolicy> {
    let strategy = serde_json::from_str::<StoredMediaStrategy>(value).ok()?;
    Some(ImageSelectionPolicy {
        poster: strategy.images.poster,
        artwork: strategy.images.artwork,
        banner: strategy.images.banner,
        logo: strategy.images.logo,
        thumbnail: strategy.images.thumbnail,
        disc: strategy.images.disc,
        wallpaper: strategy.images.wallpaper,
    })
}

fn candidate_payload(
    candidate: &StoredMetadataCandidate,
) -> Result<CandidatePayload, MetadataSelectionError> {
    if candidate.provider.trim().is_empty() || candidate.provider_id.trim().is_empty() {
        return Err(MetadataSelectionError::InvalidCandidate(
            "provider and provider ID are required".to_owned(),
        ));
    }
    let value: Value = serde_json::from_str(&candidate.candidate_json)
        .map_err(|error| MetadataSelectionError::InvalidCandidate(error.to_string()))?;
    let metadata = NfoMetadata {
        title: candidate_text(&value, &["title"]),
        original_title: candidate_text(&value, &["originalTitle", "original_title"]),
        overview: candidate_text(&value, &["overview", "plot"]),
        production_year: candidate_year(&value)?,
    };
    let tagline = candidate_text(&value, &["tagline", "Tagline"]);
    let website = candidate_text(&value, &["website", "Website", "homepage", "Homepage"]);
    let premiere_date = candidate_text(&value, &["premiereDate", "releaseDate", "release_date"]);
    let end_date = candidate_text(
        &value,
        &["endDate", "end_date", "lastAirDate", "last_air_date"],
    );
    let status = candidate_text(&value, &["status", "Status"]);
    let set_name = candidate_text(&value, &["setName", "set_name", "SetName"]);
    let set_id = candidate_text(&value, &["setId", "set_id", "SetId"]);
    let original_language = candidate_text(&value, &["originalLanguage", "original_language"]);
    let rating = candidate_rating(&value)?;
    let votes = candidate_integer(&value, &["votes", "Votes", "voteCount", "vote_count"])?;
    let runtime = candidate_integer(&value, &["runtime", "Runtime"])?
        .and_then(|value| i32::try_from(value).ok());
    let certification = candidate_text(
        &value,
        &[
            "certification",
            "Certification",
            "officialRating",
            "OfficialRating",
            "mpaa",
        ],
    );
    let countries = candidate_string_array(&value, &["countries", "Countries", "country"]);
    let genres = candidate_string_array(&value, &["genres", "Genres", "genre"]);
    let studios = candidate_string_array(&value, &["studios", "Studios", "studio"]);
    let provider_ids = candidate_provider_ids(&value)?;
    let directors = candidate_credits(&value, "directors")?;
    let writers = candidate_credits(&value, "writers")?;
    let trailers: Vec<String> =
        candidate_string_array(&value, &["trailers", "Trailers", "trailer"])
            .into_iter()
            .filter(|url| is_http_url(url))
            .collect();
    let (images, typed_images_present) = candidate_images(&value);
    let poster_url = candidate_url(&value, &["posterUrl", "poster_url", "poster"]);
    let fanart_url = candidate_url(
        &value,
        &[
            "fanartUrl",
            "fanart_url",
            "backdropUrl",
            "backdrop_url",
            "backdrop",
        ],
    );
    let actors = candidate_actors(&value)?;
    if metadata.title.is_none()
        && metadata.original_title.is_none()
        && metadata.overview.is_none()
        && metadata.production_year.is_none()
        && rating.is_none()
        && tagline.is_none()
        && website.is_none()
        && premiere_date.is_none()
        && end_date.is_none()
        && status.is_none()
        && set_name.is_none()
        && set_id.is_none()
        && original_language.is_none()
        && votes.is_none()
        && runtime.is_none()
        && certification.is_none()
        && images.values().all(Vec::is_empty)
        && poster_url.is_none()
        && fanart_url.is_none()
        && actors.is_empty()
        && directors.is_empty()
        && writers.is_empty()
        && countries.is_empty()
        && genres.is_empty()
        && studios.is_empty()
        && provider_ids.is_empty()
        && trailers.is_empty()
    {
        return Err(MetadataSelectionError::InvalidCandidate(
            "candidate contains no writable metadata or images".to_owned(),
        ));
    }
    let movie_nfo = MovieNfoMetadata {
        base: metadata.clone(),
        rating,
        votes,
        tagline,
        premiered: premiere_date.clone(),
        releasedate: premiere_date.clone(),
        runtime,
        status: status.clone(),
        original_language: original_language.clone(),
        website,
        set_name,
        set_id,
        poster_url: poster_url.clone(),
        fanart_url: fanart_url.clone(),
        certification,
        countries,
        genres,
        studios,
        provider_ids,
        directors,
        writers,
        actors: actors.clone(),
        trailers,
    };
    Ok(CandidatePayload {
        metadata,
        movie_nfo,
        premiere_date,
        end_date,
        status,
        original_language,
        rating,
        images,
        typed_images_present,
        poster_url,
        fanart_url,
        actors,
    })
}

fn candidate_integer(
    value: &Value,
    fields: &[&str],
) -> Result<Option<i64>, MetadataSelectionError> {
    let Some(raw) = fields.iter().find_map(|field| value.get(*field)) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    raw.as_i64().map(Some).ok_or_else(|| {
        MetadataSelectionError::InvalidCandidate("integer metadata field is invalid".to_owned())
    })
}

fn candidate_string_array(value: &Value, fields: &[&str]) -> Vec<String> {
    fields
        .iter()
        .find_map(|field| value.get(*field))
        .map(candidate_values)
        .unwrap_or_default()
}

fn candidate_provider_ids(
    value: &Value,
) -> Result<BTreeMap<String, String>, MetadataSelectionError> {
    let Some(raw) = value
        .get("providerIds")
        .or_else(|| value.get("provider_ids"))
    else {
        return Ok(BTreeMap::new());
    };
    let object = raw.as_object().ok_or_else(|| {
        MetadataSelectionError::InvalidCandidate("providerIds must be an object".to_owned())
    })?;
    Ok(object
        .iter()
        .filter_map(|(provider, value)| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| (provider.to_owned(), value.to_owned()))
        })
        .collect())
}

fn candidate_credits(
    value: &Value,
    field: &str,
) -> Result<Vec<MovieNfoCredit>, MetadataSelectionError> {
    let Some(raw) = value.get(field) else {
        return Ok(Vec::new());
    };
    let credits = raw.as_array().ok_or_else(|| {
        MetadataSelectionError::InvalidCandidate(format!("{field} must be an array"))
    })?;
    Ok(credits
        .iter()
        .filter_map(|credit| {
            let object = credit.as_object()?;
            let id = object
                .get("providerId")
                .or_else(|| object.get("provider_id"))
                .or_else(|| object.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)?;
            let name = object.get("name").and_then(Value::as_str)?.trim();
            (valid_person_id(id) && !name.is_empty()).then(|| MovieNfoCredit {
                provider_id: id.to_owned(),
                name: name.to_owned(),
            })
        })
        .collect())
}

fn is_http_url(value: &str) -> bool {
    let value = value.trim();
    (value.starts_with("https://") || value.starts_with("http://")) && value.len() <= 2048
}

fn candidate_rating(value: &Value) -> Result<Option<f64>, MetadataSelectionError> {
    let Some(raw) = ["rating", "Rating", "voteAverage", "vote_average"]
        .iter()
        .find_map(|field| value.get(*field))
    else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let rating = raw.as_f64().ok_or_else(|| {
        MetadataSelectionError::InvalidCandidate("rating must be a number".to_owned())
    })?;
    if !rating.is_finite() || !(0.0..=10.0).contains(&rating) {
        return Err(MetadataSelectionError::InvalidCandidate(
            "rating must be between 0 and 10".to_owned(),
        ));
    }
    Ok(Some(rating))
}

fn candidate_actors(value: &Value) -> Result<Vec<ActorCredit>, MetadataSelectionError> {
    let Some(raw) = value.get("actors") else {
        return Ok(Vec::new());
    };
    let actors = raw.as_array().ok_or_else(|| {
        MetadataSelectionError::InvalidCandidate("actors must be an array".to_owned())
    })?;
    actors
        .iter()
        .take(MAX_MOVIE_NFO_ACTORS)
        .map(|actor| {
            let actor = serde_json::from_value::<ActorCredit>(actor.clone()).map_err(|error| {
                MetadataSelectionError::InvalidCandidate(format!("actor is invalid: {error}"))
            })?;
            let id = actor.id.trim();
            let name = actor.name.trim();
            if (!id.is_empty() && !valid_person_id(id)) || name.is_empty() {
                return Err(MetadataSelectionError::InvalidCandidate(
                    "actor provider ID and name are required".to_owned(),
                ));
            }
            Ok(ActorCredit {
                id: id.to_owned(),
                provider: actor.provider,
                name: name.to_owned(),
                ..actor
            })
        })
        .collect()
}

fn candidate_images(value: &Value) -> (BTreeMap<String, Vec<String>>, bool) {
    let Some(object) = value.get("images").and_then(Value::as_object) else {
        return (BTreeMap::new(), false);
    };
    let mut images = BTreeMap::new();
    for (key, raw) in object {
        let Some(image_type) = candidate_image_type(key) else {
            continue;
        };
        let urls = candidate_values(raw);
        if !urls.is_empty() {
            images.insert(image_type.to_owned(), urls);
        }
    }
    (images, true)
}

fn candidate_image_type(value: &str) -> Option<&'static str> {
    match value.to_ascii_uppercase().as_str() {
        "POSTER" => Some("POSTER"),
        "FANART" => Some("FANART"),
        "LOGO" => Some("LOGO"),
        "THUMB" | "THUMBNAIL" => Some("THUMB"),
        "BANNER" => Some("BANNER"),
        "DISC" | "DISCART" => Some("DISC"),
        "ART" | "ARTWORK" => Some("ART"),
        "WALLPAPER" => Some("WALLPAPER"),
        _ => None,
    }
}

fn candidate_values(value: &Value) -> Vec<String> {
    match value {
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect(),
        Value::String(value) if !value.trim().is_empty() => vec![value.trim().to_owned()],
        _ => Vec::new(),
    }
}

fn candidate_text(value: &Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| {
        value
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    })
}

fn candidate_year(value: &Value) -> Result<Option<i32>, MetadataSelectionError> {
    let raw = value
        .get("productionYear")
        .or_else(|| value.get("production_year"))
        .or_else(|| value.get("release_date"));
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let year = if let Some(year) = raw.as_i64() {
        i32::try_from(year).ok()
    } else {
        raw.as_str()
            .and_then(|value| value.get(..4))
            .and_then(|value| value.parse::<i32>().ok())
    };
    match year {
        Some(year) if (1800..=2200).contains(&year) => Ok(Some(year)),
        _ => Err(MetadataSelectionError::InvalidCandidate(
            "production year is invalid".to_owned(),
        )),
    }
}

fn candidate_url(value: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| is_http_url(value))
        .map(str::to_owned)
}

fn candidate_view(
    row: StoredMetadataCandidate,
    current: Option<&StoredMediaMetadata>,
) -> Result<MetadataCandidateView, MetadataCandidateError> {
    let candidate: Value = serde_json::from_str(&row.candidate_json)
        .map_err(|error| MetadataCandidateError::InvalidCandidateJson(error.to_string()))?;
    let field_diffs = current
        .map(|current| field_diffs(current, &candidate))
        .unwrap_or_default();
    Ok(MetadataCandidateView {
        id: row.id,
        item_id: row.item_id,
        item_title: row.item_title,
        provider: row.provider,
        provider_id: row.provider_id,
        candidate,
        score: row.score,
        status: row.status,
        expires_at: row.expires_at,
        field_diffs,
    })
}

fn field_diffs(current: &StoredMediaMetadata, candidate: &Value) -> Vec<MetadataFieldDiff> {
    let provenance =
        serde_json::from_str::<Value>(current.provenance_json.as_deref().unwrap_or("{}"))
            .unwrap_or_else(|_| json!({}));
    let fields = [
        (
            "title",
            Value::String(current.title.clone()),
            candidate_value(candidate, "title"),
        ),
        (
            "originalTitle",
            optional_string_value(current.original_title.as_deref()),
            candidate_value_alias(candidate, &["originalTitle", "original_title"]),
        ),
        (
            "overview",
            optional_string_value(current.overview.as_deref()),
            candidate_value(candidate, "overview"),
        ),
        (
            "productionYear",
            current
                .production_year
                .map(Value::from)
                .unwrap_or(Value::Null),
            candidate_production_year(candidate),
        ),
    ];
    fields
        .into_iter()
        .filter_map(|(field, current, candidate)| {
            let candidate = candidate?;
            (current != candidate).then(|| MetadataFieldDiff {
                field: field.to_owned(),
                current,
                candidate,
                provenance: provenance
                    .get(field)
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect()
}

fn optional_string_value(value: Option<&str>) -> Value {
    value.map(Value::from).unwrap_or(Value::Null)
}

fn candidate_value(candidate: &Value, field: &str) -> Option<Value> {
    candidate.get(field).and_then(|value| {
        (!value.is_null()).then(|| {
            value
                .as_str()
                .map(|value| Value::String(value.trim().to_owned()))
                .unwrap_or_else(|| value.clone())
        })
    })
}

fn candidate_value_alias(candidate: &Value, fields: &[&str]) -> Option<Value> {
    fields
        .iter()
        .find_map(|field| candidate_value(candidate, field))
}

fn candidate_production_year(candidate: &Value) -> Option<Value> {
    if let Some(value) = candidate_value_alias(candidate, &["productionYear", "production_year"]) {
        return Some(value);
    }
    candidate
        .get("release_date")
        .and_then(Value::as_str)
        .and_then(|value| value.get(..4))
        .and_then(|value| value.parse::<i64>().ok())
        .map(Value::from)
}

#[cfg(test)]
mod tests {
    use super::{
        ACTOR_METADATA_FETCH_CONCURRENCY, candidate_actors, credits_are_missing,
        default_image_selection_policy, enrich_actor_metadata, generic_candidate_images,
        merge_supplemental_movie_nfo, metadata_match_score, metadata_request_plan,
    };
    use crate::application::scraper::{
        ScraperAdapter, ScraperCreditsResponse, ScraperError, ScraperExternalIdsResponse,
        ScraperFuture, ScraperGetRequest, ScraperImage, ScraperImageRequest, ScraperImagesResponse,
        ScraperItemType, ScraperMetadata, ScraperMetadataBundle, ScraperProvider,
        ScraperSearchRequest, ScraperSearchResponse, ScraperTrailersResponse,
    };
    use crate::storage::StoredMediaMetadata;
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use tokio::time::{Duration, sleep};

    #[derive(Clone)]
    struct DelayedActorAdapter {
        active: Arc<AtomicUsize>,
        maximum: Arc<AtomicUsize>,
    }

    impl ScraperAdapter for DelayedActorAdapter {
        fn provider_key(&self) -> &str {
            "tmdb"
        }

        fn search(
            &self,
            _request: ScraperSearchRequest,
        ) -> ScraperFuture<'_, Result<ScraperSearchResponse, ScraperError>> {
            Box::pin(std::future::ready(Ok(ScraperSearchResponse::default())))
        }

        fn get(
            &self,
            request: ScraperGetRequest,
        ) -> ScraperFuture<'_, Result<ScraperMetadata, ScraperError>> {
            let active = Arc::clone(&self.active);
            let maximum = Arc::clone(&self.maximum);
            Box::pin(async move {
                let active_count = active.fetch_add(1, Ordering::SeqCst) + 1;
                maximum.fetch_max(active_count, Ordering::SeqCst);
                sleep(Duration::from_millis(20)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                Ok(ScraperMetadata {
                    item_type: Some("Person".to_owned()),
                    title: Some(request.provider_id),
                    overview: Some("Biography".to_owned()),
                    ..ScraperMetadata::default()
                })
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
            _request: ScraperImageRequest,
        ) -> ScraperFuture<'_, Result<ScraperImagesResponse, ScraperError>> {
            Box::pin(std::future::ready(Ok(ScraperImagesResponse::default())))
        }

        fn credits(
            &self,
            _request: ScraperGetRequest,
        ) -> ScraperFuture<'_, Result<ScraperCreditsResponse, ScraperError>> {
            Box::pin(std::future::ready(Ok(ScraperCreditsResponse::default())))
        }

        fn external_ids(
            &self,
            _request: ScraperGetRequest,
        ) -> ScraperFuture<'_, Result<ScraperExternalIdsResponse, ScraperError>> {
            Box::pin(std::future::ready(
                Ok(ScraperExternalIdsResponse::default()),
            ))
        }

        fn trailers(
            &self,
            _request: ScraperGetRequest,
        ) -> ScraperFuture<'_, Result<ScraperTrailersResponse, ScraperError>> {
            Box::pin(std::future::ready(Ok(ScraperTrailersResponse::default())))
        }
    }

    #[tokio::test]
    async fn actor_metadata_fetches_are_bounded_and_parallel() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let scraper = ScraperProvider::from_adapter(DelayedActorAdapter {
            active: Arc::clone(&active),
            maximum: Arc::clone(&maximum),
        });
        let mut actors = (0..8)
            .map(|index| super::ActorCredit {
                id: index.to_string(),
                provider: Some("tmdb".to_owned()),
                identities: Vec::new(),
                name: format!("Actor {index}"),
                character: None,
                order: Some(index),
                profile_url: None,
                person: None,
            })
            .collect::<Vec<_>>();

        enrich_actor_metadata(&scraper, &mut actors).await;

        assert!(actors.iter().all(|actor| actor.person.is_some()));
        assert!(maximum.load(Ordering::SeqCst) > 1);
        assert!(maximum.load(Ordering::SeqCst) <= ACTOR_METADATA_FETCH_CONCURRENCY);
    }

    #[test]
    fn metadata_refresh_keeps_backdrop_images_as_fanart() {
        let enabled_types = default_image_selection_policy()
            .enabled_types()
            .collect::<Vec<_>>();

        assert!(enabled_types.contains(&"FANART"));
    }

    #[test]
    fn episode_stills_are_not_duplicated_into_other_backdrop_types() {
        let images = generic_candidate_images(
            &[ScraperImage {
                image_type: "Backdrop".to_owned(),
                url: "https://images.example/episode.jpg".to_owned(),
                ..ScraperImage::default()
            }],
            ScraperItemType::Episode,
        );

        assert_eq!(
            images.get("FANART"),
            Some(&vec!["https://images.example/episode.jpg".to_owned()])
        );
        assert!(!images.contains_key("THUMB"));
        assert!(!images.contains_key("BANNER"));
    }

    #[test]
    fn fill_missing_request_plan_only_keeps_missing_capabilities() {
        let current = StoredMediaMetadata {
            item_type: "MOVIE".to_owned(),
            title: "Example Movie".to_owned(),
            original_title: Some("Example Movie".to_owned()),
            overview: Some("Overview".to_owned()),
            production_year: Some(2020),
            premiere_date: Some("2020-01-01".to_owned()),
            last_air_date: None,
            status: None,
            original_language: Some("en".to_owned()),
            rating: Some(8.0),
            provider_ids_json: Some(json!({"tmdb": "1", "imdb": "tt1"}).to_string()),
            metadata_scraper_id: Some("tmdb".to_owned()),
            scraper_id: Some("tmdb".to_owned()),
            provenance_json: Some(
                json!({
                    "title": "LOCAL_NFO",
                    "originalTitle": "LOCAL_NFO",
                    "overview": "LOCAL_NFO",
                    "productionYear": "LOCAL_NFO"
                })
                .to_string(),
            ),
            locked_fields_json: Some("[]".to_owned()),
            nfo_metadata_json: Some(
                json!({
                    "rating": 8.0,
                    "releaseDate": "2020-01-01",
                    "originalLanguage": "en",
                    "trailers": ["https://example.invalid/trailer"]
                })
                .to_string(),
            ),
            series_item_id: None,
            series_title: None,
            series_production_year: None,
            series_provider_name: None,
            series_provider_id: None,
            season_number: None,
            episode_number: None,
        };

        let details = crate::application::nfo::LocalNfoDetails {
            trailers: vec!["https://example.invalid/trailer".to_owned()],
            ..crate::application::nfo::LocalNfoDetails::default()
        };
        let plan = metadata_request_plan(&current, true, false, Some(&details));
        assert!(!plan.needs_metadata);
        assert!(plan.needs_images);
        assert!(!plan.needs_credits);
        assert!(!plan.needs_external_ids);
        assert!(!plan.needs_trailers);
    }

    #[test]
    fn locked_missing_fields_do_not_keep_metadata_requests_pending() {
        let current = StoredMediaMetadata {
            item_type: "MOVIE".to_owned(),
            title: "Example Movie".to_owned(),
            original_title: Some("Example Movie".to_owned()),
            overview: None,
            production_year: Some(2020),
            premiere_date: Some("2020-01-01".to_owned()),
            last_air_date: None,
            status: None,
            original_language: Some("en".to_owned()),
            rating: Some(8.0),
            provider_ids_json: Some(json!({"tmdb": "1", "imdb": "tt1"}).to_string()),
            metadata_scraper_id: Some("tmdb".to_owned()),
            scraper_id: Some("tmdb".to_owned()),
            provenance_json: Some(
                json!({
                    "title": "LOCAL_NFO",
                    "originalTitle": "LOCAL_NFO",
                    "productionYear": "LOCAL_NFO"
                })
                .to_string(),
            ),
            locked_fields_json: Some(json!(["overview"]).to_string()),
            nfo_metadata_json: None,
            series_item_id: None,
            series_title: None,
            series_production_year: None,
            series_provider_name: None,
            series_provider_id: None,
            season_number: None,
            episode_number: None,
        };
        let details = crate::application::nfo::LocalNfoDetails {
            directors: vec![crate::application::nfo::LocalNfoCredit {
                provider_id: "director-1".to_owned(),
                name: "Director".to_owned(),
            }],
            writers: vec![crate::application::nfo::LocalNfoCredit {
                provider_id: "writer-1".to_owned(),
                name: "Writer".to_owned(),
            }],
            trailers: vec!["https://example.invalid/trailer".to_owned()],
            ..crate::application::nfo::LocalNfoDetails::default()
        };

        let plan = metadata_request_plan(&current, false, false, Some(&details));
        assert!(!plan.needs_metadata);
    }

    #[test]
    fn fill_missing_fetches_credits_when_one_crew_list_is_missing() {
        let details = crate::application::nfo::LocalNfoDetails {
            directors: vec![crate::application::nfo::LocalNfoCredit {
                provider_id: "director-1".to_owned(),
                name: "Director".to_owned(),
            }],
            ..crate::application::nfo::LocalNfoDetails::default()
        };

        assert!(credits_are_missing(true, Some(&details)));
    }

    #[test]
    fn scraper_cast_becomes_ordered_candidate_actor_data() {
        let actors = candidate_actors(&json!({
            "actors": [
                {
                    "id": "person-9",
                    "provider": "douban",
                    "name": " 演员甲 ",
                    "character": "角色甲",
                    "profileUrl": "https://images.example/profile.jpg",
                    "order": 0
                },
                {"id": "person-10", "name": "演员乙", "order": 1}
            ]
        }))
        .expect("scraper cast should parse");

        assert_eq!(actors[0].name, "演员甲");
        assert_eq!(actors[0].character.as_deref(), Some("角色甲"));
        assert_eq!(
            actors[0].profile_url.as_deref(),
            Some("https://images.example/profile.jpg")
        );
        assert_eq!(actors[1].id, "person-10");
    }

    #[test]
    fn candidate_actors_allow_provider_scoped_ids() {
        let result = candidate_actors(&json!({
            "actors": [{"id": "person-9", "name": "演员甲"}]
        }));

        let actors = result.expect("provider-scoped actor ID");
        assert_eq!(actors[0].id, "person-9");
    }

    #[test]
    fn candidate_actors_allow_missing_provider_ids() {
        let result = candidate_actors(&json!({
            "actors": [{"name": "本地演员", "character": "本地角色"}]
        }))
        .expect("actor name is enough for display");

        assert_eq!(result.len(), 1);
        assert!(result[0].id.is_empty());
        assert_eq!(result[0].name, "本地演员");
    }

    #[test]
    fn metadata_match_score_requires_title_agreement_before_year_bonus() {
        assert_eq!(
            metadata_match_score(
                "Example Movie",
                Some(2020),
                Some("Other Movie"),
                None,
                Some(2020),
            ),
            0.0
        );
    }

    #[test]
    fn metadata_match_score_keeps_missing_year_below_auto_match_threshold() {
        assert_eq!(
            metadata_match_score("Example Movie", None, Some("Example Movie"), None, None),
            65.0
        );
    }

    #[test]
    fn metadata_match_score_rewards_same_or_nearby_years() {
        assert_eq!(
            metadata_match_score(
                "Example Movie",
                Some(2020),
                Some("Example Movie"),
                None,
                Some(2020),
            ),
            95.0
        );
        assert_eq!(
            metadata_match_score(
                "Example Movie",
                Some(2020),
                Some("Example Movie"),
                None,
                Some(2021),
            ),
            85.0
        );
        assert_eq!(
            metadata_match_score(
                "Example Movie",
                Some(2020),
                Some("Example Movie"),
                None,
                Some(2015),
            ),
            45.0
        );
    }

    #[test]
    fn supplemental_merge_appends_unique_lists_and_keeps_existing_first() {
        let mut candidate = crate::application::nfo::MovieNfoMetadata {
            genres: vec!["动作".to_owned(), "科幻".to_owned()],
            studios: vec!["主制作公司".to_owned()],
            directors: vec![crate::application::nfo::MovieNfoCredit {
                provider_id: "director-1".to_owned(),
                name: "主导演".to_owned(),
            }],
            actors: vec![super::ActorCredit {
                id: "actor-2".to_owned(),
                provider: Some("supplement".to_owned()),
                identities: Vec::new(),
                name: "补充演员".to_owned(),
                character: None,
                order: Some(1),
                profile_url: None,
                person: None,
            }],
            provider_ids: [
                ("tmdb".to_owned(), "main-1".to_owned()),
                ("imdb".to_owned(), "tt-supplement".to_owned()),
            ]
            .into_iter()
            .collect(),
            trailers: vec!["https://video.example/supplement".to_owned()],
            ..Default::default()
        };
        let existing = crate::application::nfo::LocalNfoProjection {
            details: crate::application::nfo::LocalNfoDetails {
                genres: vec!["动作".to_owned(), "本地类型".to_owned()],
                studios: vec!["主制作公司".to_owned(), "本地制作公司".to_owned()],
                directors: vec![crate::application::nfo::LocalNfoCredit {
                    provider_id: "director-1".to_owned(),
                    name: "主导演".to_owned(),
                }],
                provider_ids: [("tmdb".to_owned(), "local-1".to_owned())]
                    .into_iter()
                    .collect(),
                trailers: vec!["https://video.example/main".to_owned()],
                ..Default::default()
            },
            actors: vec![super::ActorCredit {
                id: "actor-1".to_owned(),
                provider: Some("main".to_owned()),
                identities: Vec::new(),
                name: "主演员".to_owned(),
                character: None,
                order: Some(0),
                profile_url: None,
                person: None,
            }],
            ..Default::default()
        };

        merge_supplemental_movie_nfo(&mut candidate, Some(&existing), true);

        assert_eq!(candidate.genres, ["动作", "本地类型", "科幻"]);
        assert_eq!(candidate.studios, ["主制作公司", "本地制作公司"]);
        assert_eq!(candidate.directors.len(), 1);
        assert_eq!(
            candidate.trailers,
            [
                "https://video.example/main",
                "https://video.example/supplement"
            ]
        );
        assert_eq!(candidate.actors.len(), 2);
        assert_eq!(candidate.provider_ids["tmdb"], "main-1");
        assert_eq!(candidate.provider_ids["imdb"], "tt-supplement");
    }
}
