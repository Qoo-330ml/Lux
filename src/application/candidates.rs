use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{sync::Semaphore, task::JoinSet};

use crate::{
    application::{
        home::HomeService,
        images::{ImageWriteError, ImageWriteService, MAX_IMAGE_VARIANTS, image_no_candidate_key},
        media_matching::{MediaKind, parse_media_name, title_candidates},
        metadata::{MetadataCandidate, MetadataField, MetadataSource, MetadataState, NfoMetadata},
        nfo::{MovieNfoCredit, MovieNfoMetadata, NfoWriteError, NfoWriteService},
        people::{ActorCredit, PeopleError},
        scraper::{
            ScraperError, ScraperGetRequest, ScraperImageRequest, ScraperItemType, ScraperMetadata,
            ScraperProvider, ScraperSearchResponse, ScraperSearchResult, provider_id_for_key,
            provider_key_from_plugin_id,
        },
        thumbnail_policy::ThumbnailScrapingMode,
    },
    observability::resources::ResourceMetrics,
    storage::{
        Database, MetadataCapabilityResult, MetadataImageUnavailable, NewMetadataCandidate,
        SelectedMetadataUpdate, StorageError, StoredMediaMetadata, StoredMetadataCandidate,
        StoredMetadataCapabilityAttempt,
    },
};

const MAX_MOVIE_NFO_ACTORS: usize = 100;
// Keep optional per-person RPCs bounded while retaining the full cast list.
const MAX_ACTOR_DETAIL_FETCHES: usize = 12;
const ACTOR_METADATA_FETCH_CONCURRENCY: usize = 4;
const IMAGE_ITEM_CONCURRENCY: usize = 4;
const COMPLETENESS_PLAN_ITEM_CONCURRENCY: usize = 4;
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
const CANDIDATE_METADATA_DETAILS_VERSION: u64 = 2;
const COMPLETENESS_INPUT_VERSION: u64 = 1;

type CompletenessPlanTask<T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'static>>;

async fn run_bounded_completeness_plan_tasks<T: Send + 'static>(
    tasks: Vec<(usize, CompletenessPlanTask<T>)>,
) -> Result<Vec<(usize, T)>, tokio::task::JoinError> {
    let mut queued = tasks.into_iter();
    let mut pending = JoinSet::new();
    let mut results = Vec::new();

    while pending.len() < COMPLETENESS_PLAN_ITEM_CONCURRENCY {
        let Some((index, task)) = queued.next() else {
            break;
        };
        pending.spawn(async move { (index, task.await) });
    }
    while let Some(result) = pending.join_next().await {
        results.push(result?);
        if let Some((index, task)) = queued.next() {
            pending.spawn(async move { (index, task.await) });
        }
    }
    results.sort_unstable_by_key(|(index, _)| *index);
    Ok(results)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct MetadataRequestPlan {
    pub(crate) needs_metadata: bool,
    pub(crate) needs_images: bool,
    pub(crate) needs_credits: bool,
    pub(crate) needs_external_ids: bool,
    pub(crate) needs_trailers: bool,
    pub(crate) image_policy: Option<ImageSelectionPolicy>,
    pub(crate) missing_image_mask: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalMetadataCompletenessPlan {
    pub(crate) input_fingerprint: Vec<u8>,
    pub(crate) capabilities: Vec<(String, bool)>,
    pub(crate) has_requestable_capability: bool,
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
            missing_image_mask: 0,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct FillMissingRequestPlan {
    pub(crate) requestable: MetadataRequestPlan,
    pub(crate) actual_missing_image_mask: u16,
}

#[derive(Clone, Copy)]
enum MetadataAttemptMode {
    RespectRetryState,
    IgnoreRetryState,
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
        missing_image_mask: 0,
    }
}

fn completeness_capabilities(
    current: &StoredMediaMetadata,
    plan: MetadataRequestPlan,
) -> Vec<(String, bool)> {
    if fill_missing_fields(&current.item_type).is_none() {
        return Vec::new();
    }
    let mut capabilities = vec![("METADATA".to_owned(), plan.needs_metadata)];
    if let Some(image_policy) = plan.image_policy {
        for image_type in image_policy.enabled_types() {
            let missing = SCRAPER_IMAGE_TYPES
                .iter()
                .position(|candidate| *candidate == image_type)
                .is_some_and(|index| plan.missing_image_mask & (1_u16 << index) != 0);
            capabilities.push((image_type.to_owned(), missing));
        }
    }
    if matches!(current.item_type.as_str(), "MOVIE" | "SERIES") {
        capabilities.push(("CREDITS".to_owned(), plan.needs_credits));
        capabilities.push(("TRAILERS".to_owned(), plan.needs_trailers));
    }
    if current.item_type == "MOVIE" {
        capabilities.push(("EXTERNAL_IDS".to_owned(), plan.needs_external_ids));
    }
    capabilities
}

fn metadata_completeness_fingerprint(
    item_id: &str,
    current: &StoredMediaMetadata,
    actual_plan: MetadataRequestPlan,
    requestable_plan: MetadataRequestPlan,
) -> Vec<u8> {
    let input = json!({
        "version": COMPLETENESS_INPUT_VERSION,
        "itemId": item_id,
        "current": {
            "itemType": current.item_type,
            "providerIds": normalized_json(current.provider_ids_json.as_deref()),
            "metadataScraperId": current.metadata_scraper_id,
            "scraperId": current.scraper_id,
            "identificationStatus": current.identification_status,
            "provenance": normalized_json(current.provenance_json.as_deref()),
            "lockedFields": normalized_json(current.locked_fields_json.as_deref()),
            "seriesItemId": current.series_item_id,
            "seriesProviderName": current.series_provider_name,
            "seriesProviderId": current.series_provider_id,
            "seasonNumber": current.season_number,
            "episodeNumber": current.episode_number,
        },
        "actualPlan": request_plan_json(actual_plan),
        "requestablePlan": request_plan_json(requestable_plan),
    });
    let serialized = serde_json::to_vec(&input).unwrap_or_default();
    Sha256::digest(&serialized).to_vec()
}

fn normalized_json(value: Option<&str>) -> Value {
    value
        .and_then(|value| serde_json::from_str(value).ok())
        .unwrap_or(Value::Null)
}

fn request_plan_json(plan: MetadataRequestPlan) -> Value {
    json!({
        "needsMetadata": plan.needs_metadata,
        "needsImages": plan.needs_images,
        "needsCredits": plan.needs_credits,
        "needsExternalIds": plan.needs_external_ids,
        "needsTrailers": plan.needs_trailers,
        "imagePolicy": plan.image_policy.map(|policy| json!({
            "poster": policy.poster,
            "artwork": policy.artwork,
            "banner": policy.banner,
            "logo": policy.logo,
            "thumbnail": policy.thumbnail,
            "disc": policy.disc,
            "wallpaper": policy.wallpaper,
            "thumbnailScrapingMode": format!("{:?}", policy.thumbnail_scraping_mode),
        })),
        "missingImageMask": plan.missing_image_mask,
    })
}

fn metadata_request_plan_has_work(plan: MetadataRequestPlan) -> bool {
    plan.needs_metadata
        || plan.needs_images
        || plan.needs_credits
        || plan.needs_external_ids
        || plan.needs_trailers
}

fn automatic_fill_missing_plan_has_work(plan: MetadataRequestPlan) -> bool {
    // Optional provider extras are collected when real fill work runs, but do not
    // start an automatic scan job by themselves.
    plan.needs_metadata || plan.needs_images || plan.needs_credits
}

fn missing_image_mask(image_types: &[&str], local_image_types: &BTreeSet<String>) -> u16 {
    image_types.iter().fold(0_u16, |mask, image_type| {
        if local_image_types.contains(*image_type) {
            return mask;
        }
        SCRAPER_IMAGE_TYPES
            .iter()
            .position(|candidate| *candidate == *image_type)
            .map_or(mask, |index| mask | (1_u16 << index))
    })
}

fn requestable_metadata_plan(
    actual_plan: MetadataRequestPlan,
    image_types: &[&str],
    capability_identity: Option<&(String, String)>,
    attempts: Option<&(
        Vec<StoredMetadataCapabilityAttempt>,
        Vec<crate::storage::StoredMetadataImageAttempt>,
    )>,
) -> MetadataRequestPlan {
    let (capability_states, image_attempts) =
        attempts.map_or((&[][..], &[][..]), |(capability_states, image_attempts)| {
            (capability_states.as_slice(), image_attempts.as_slice())
        });
    let unavailable_image_attempts = image_attempts
        .iter()
        .filter(|attempt| attempt.status.eq_ignore_ascii_case("UNAVAILABLE"))
        .map(|attempt| (attempt.image_type.clone(), attempt.candidate_key.clone()))
        .collect::<BTreeSet<_>>();
    let image_attempt_identities = if attempts.is_some() {
        image_attempt_identities(capability_identity)
    } else {
        Vec::new()
    };
    let mut requestable_missing_image_mask = actual_plan.missing_image_mask;
    for image_type in image_types {
        let Some(index) = SCRAPER_IMAGE_TYPES
            .iter()
            .position(|candidate| *candidate == *image_type)
        else {
            continue;
        };
        let image_mask = 1_u16 << index;
        if actual_plan.missing_image_mask & image_mask == 0 {
            continue;
        }
        let explicitly_unavailable = !image_attempt_identities.is_empty()
            && image_attempt_identities
                .iter()
                .all(|(source, provider_id)| {
                    unavailable_image_attempts.contains(&(
                        (*image_type).to_owned(),
                        image_no_candidate_key(source, image_type, provider_id),
                    ))
                });
        if explicitly_unavailable {
            requestable_missing_image_mask &= !image_mask;
        }
    }
    let mut requestable_plan = actual_plan;
    requestable_plan.needs_images = requestable_missing_image_mask != 0;
    requestable_plan.missing_image_mask = requestable_missing_image_mask;
    requestable_plan.needs_credits = requestable_plan.needs_credits
        && capability_needs_request(capability_states, capability_identity, CAPABILITY_CREDITS);
    requestable_plan.needs_external_ids = requestable_plan.needs_external_ids
        && capability_needs_request(
            capability_states,
            capability_identity,
            CAPABILITY_EXTERNAL_IDS,
        );
    requestable_plan.needs_trailers = requestable_plan.needs_trailers
        && capability_needs_request(capability_states, capability_identity, CAPABILITY_TRAILERS);
    requestable_plan
}

fn local_metadata_completeness_plan(
    item_id: &str,
    current: &StoredMediaMetadata,
    actual_plan: MetadataRequestPlan,
    requestable_plan: MetadataRequestPlan,
) -> Option<LocalMetadataCompletenessPlan> {
    fill_missing_fields(&current.item_type)?;
    Some(LocalMetadataCompletenessPlan {
        input_fingerprint: metadata_completeness_fingerprint(
            item_id,
            current,
            actual_plan,
            requestable_plan,
        ),
        capabilities: completeness_capabilities(current, actual_plan),
        has_requestable_capability: automatic_fill_missing_plan_has_work(requestable_plan),
    })
}

fn credits_need_actor_relation_check(
    details: Option<&crate::application::nfo::LocalNfoDetails>,
) -> bool {
    details.is_some_and(|value| !value.directors.is_empty() && !value.writers.is_empty())
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
        let item_ids = rows
            .iter()
            .map(|row| row.item_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let current_by_item = self
            .database
            .list_media_item_metadata_by_ids(&item_ids)
            .await?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            let current = current_by_item.get(&row.item_id);
            items.push(candidate_view(row, current)?);
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

    async fn list_pending_for_current(
        &self,
        item_id: &str,
        current: &StoredMediaMetadata,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        let limit = 50;
        let (rows, total) = self
            .database
            .list_pending_metadata_candidates_for_item_with_count(item_id, 0, limit)
            .await?;
        let items = rows
            .into_iter()
            .map(|row| candidate_view(row, Some(current)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(MetadataCandidatePage {
            items,
            total,
            offset: 0,
            limit,
        })
    }

    async fn list_pending_after_search(
        &self,
        item_id: &str,
        current: &StoredMediaMetadata,
        reuse_current_snapshot: bool,
        automatic_match: bool,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        let refreshed_current;
        let current = if reuse_current_snapshot {
            current
        } else {
            refreshed_current = self
                .database
                .find_media_item_metadata(item_id)
                .await?
                .ok_or(MetadataCandidateError::ItemNotFound)?;
            &refreshed_current
        };
        if automatic_match {
            let limit = 50;
            let (rows, total) = self
                .database
                .list_best_pending_metadata_candidates_for_item(item_id, limit, 2)
                .await?;
            let items = rows
                .into_iter()
                .map(|row| candidate_view(row, Some(current)))
                .collect::<Result<Vec<_>, _>>()?;
            return Ok(MetadataCandidatePage {
                items,
                total,
                offset: 0,
                limit,
            });
        }
        self.list_pending_for_current(item_id, current).await
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
            CandidateSearchOptions {
                mode: CandidateSearchMode::Manual,
                plan: MetadataRequestPlan::full(),
                current: None,
            },
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
            CandidateSearchOptions {
                mode: CandidateSearchMode::AutomaticReuse,
                plan,
                current: None,
            },
        )
        .await
    }

    pub(crate) async fn search_and_store_for_automatic_match_with_plan_for_current(
        &self,
        item_id: &str,
        query: &str,
        year: Option<i32>,
        scraper: &ScraperProvider,
        plan: MetadataRequestPlan,
        current: &StoredMediaMetadata,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        self.search_and_store_with_mode(
            item_id,
            query,
            year,
            scraper,
            CandidateSearchOptions {
                mode: CandidateSearchMode::AutomaticReuse,
                plan,
                current: Some(current),
            },
        )
        .await
    }

    pub(crate) async fn search_and_store_for_automatic_match_fresh_for_current(
        &self,
        item_id: &str,
        query: &str,
        year: Option<i32>,
        scraper: &ScraperProvider,
        current: &StoredMediaMetadata,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        self.search_and_store_with_mode(
            item_id,
            query,
            year,
            scraper,
            CandidateSearchOptions {
                mode: CandidateSearchMode::AutomaticFresh,
                plan: MetadataRequestPlan::full(),
                current: Some(current),
            },
        )
        .await
    }

    async fn search_and_store_with_mode(
        &self,
        item_id: &str,
        query: &str,
        year: Option<i32>,
        scraper: &ScraperProvider,
        options: CandidateSearchOptions<'_>,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        let CandidateSearchOptions {
            mode,
            plan,
            current,
        } = options;
        let automatic_match = matches!(
            mode,
            CandidateSearchMode::AutomaticReuse | CandidateSearchMode::AutomaticFresh
        );
        let reuse_current_snapshot = current.is_some();
        let fetched_current = if current.is_none() {
            Some(
                self.database
                    .find_media_item_metadata(item_id)
                    .await?
                    .ok_or(MetadataCandidateError::ItemNotFound)?,
            )
        } else {
            None
        };
        let current = current
            .or(fetched_current.as_ref())
            .ok_or(MetadataCandidateError::ItemNotFound)?;
        if matches!(mode, CandidateSearchMode::AutomaticReuse)
            && let Some(page) = self
                .reuse_unexpired_automatic_candidates(item_id, current, scraper, plan)
                .await?
        {
            return Ok(page);
        }
        let kind = match current.item_type.as_str() {
            "MOVIE" => MediaKind::Movie,
            "SERIES" => MediaKind::Series,
            "SEASON" | "EPISODE" => {
                return self
                    .search_child_and_store(
                        item_id,
                        query,
                        year,
                        scraper,
                        CandidateSearchContext {
                            current,
                            plan,
                            reuse_current_snapshot,
                            automatic_match,
                        },
                    )
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
        let direct_provider_id = selected_scraper_provider_id(current, scraper).filter(|_| {
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
            let mut provider_ids = current_provider_ids(current);
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
        let automatic_search = matches!(
            mode,
            CandidateSearchMode::AutomaticReuse | CandidateSearchMode::AutomaticFresh
        );
        let mut results = response
            .items
            .into_iter()
            .take(20)
            .map(|result| {
                let score = automatic_search.then(|| search_result_score(current, &result));
                (result, score)
            })
            .collect::<Vec<_>>();
        if automatic_search {
            results.sort_by(|(_, left_score), (_, right_score)| {
                right_score
                    .unwrap_or_default()
                    .total_cmp(&left_score.unwrap_or_default())
            });
            results.truncate(2);
        }
        let mut pending_candidates = Vec::with_capacity(results.len());
        for (result_index, (result, precomputed_score)) in results.into_iter().enumerate() {
            let Some((provider, provider_id)) = scraper.selected_provider_entry(&result) else {
                continue;
            };
            let provider = provider.to_owned();
            let provider_id = provider_id.to_owned();
            if matches!(
                mode,
                CandidateSearchMode::AutomaticReuse | CandidateSearchMode::AutomaticFresh
            ) && result_index > 0
                && !plan.needs_metadata
                && plan != MetadataRequestPlan::full()
            {
                let score =
                    precomputed_score.unwrap_or_else(|| search_result_score(current, &result));
                let mut provider_ids = result.provider_ids.clone();
                provider_ids
                    .entry(scraper.provider_key().to_owned())
                    .or_insert_with(|| provider_id.clone());
                pending_candidates.push((
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
                ));
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
                Some(
                    scraper
                        .get_generic(crate::application::scraper::ScraperGetRequest::new(
                            item_type,
                            provider_id.clone(),
                            "zh-CN",
                        ))
                        .await
                        .map_err(MetadataCandidateError::Scraper)?,
                )
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
            let image_original_language = details
                .as_ref()
                .and_then(|value| value.original_language.clone())
                .or_else(|| result.original_language.clone());
            let (images_response, credits, external_ids, trailers) = if let Some(bundle) = bundle {
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
                    Some(bundle_images),
                    bundle.credits,
                    Some(bundle.external_ids),
                    bundle
                        .trailers
                        .trailers
                        .into_iter()
                        .filter_map(|trailer| trailer.url)
                        .collect(),
                )
            } else {
                let (images_response, credits_result, external_ids_result, trailers_result) = tokio::join!(
                    async {
                        if plan.needs_images {
                            let mut image_request =
                                crate::application::scraper::ScraperImageRequest::new(
                                    item_type,
                                    provider_id.clone(),
                                    "zh-CN",
                                );
                            image_request.original_language = image_original_language.clone();
                            scraper.images_generic(image_request).await.ok()
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
                (images_response, credits, external_ids, trailers)
            };
            let now = current_unix_timestamp();
            let mut transient_capability_failures = Vec::new();
            for (capability, error) in capability_failures {
                if capability_error_is_permanent(&error) {
                    capability_results.push(MetadataCapabilityResult {
                        capability,
                        has_data: false,
                    });
                } else {
                    transient_capability_failures.push(capability);
                }
            }
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
            if !transient_capability_failures.is_empty() {
                self.database
                    .record_metadata_capability_failures(
                        item_id,
                        scraper.provider_key(),
                        &provider_id,
                        &transient_capability_failures,
                        now,
                    )
                    .await
                    .map_err(MetadataCandidateError::Storage)?;
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
            pending_candidates.push((
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
                    images: images_response
                        .as_ref()
                        .map(|response| generic_candidate_images(&response.images, item_type))
                        .unwrap_or_default(),
                    actors,
                    metadata_fetched: details.is_some(),
                    score: direct_provider_id.as_ref().map(|_| 100.0),
                },
                expires_at,
            ));
        }
        self.store_candidates(item_id, current, pending_candidates)
            .await?;
        self.list_pending_after_search(item_id, current, reuse_current_snapshot, automatic_match)
            .await
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
            .list_unexpired_pending_metadata_candidates_for_item(item_id, &provider_key, 50)
            .await?;
        rows.retain(|row| provider_key_from_plugin_id(&row.provider) == provider_key);
        rows.truncate(2);
        if rows.is_empty() {
            return Ok(None);
        }
        let mut candidate_values = Vec::with_capacity(rows.len());
        if plan.needs_metadata {
            let item_type = match current.item_type.as_str() {
                "MOVIE" => ScraperItemType::Movie,
                "SERIES" => ScraperItemType::Series,
                _ => return Ok(None),
            };
            for row in &mut rows {
                let mut value =
                    serde_json::from_str::<Value>(&row.candidate_json).map_err(|error| {
                        MetadataCandidateError::InvalidCandidateJson(error.to_string())
                    })?;
                if !Self::candidate_metadata_was_fetched(&value) {
                    let details = scraper
                        .get_generic(ScraperGetRequest::new(
                            item_type,
                            row.provider_id.clone(),
                            "zh-CN",
                        ))
                        .await
                        .map_err(MetadataCandidateError::Scraper)?;
                    value = Self::merge_scraper_metadata_into_candidate(value, &details)?;
                    let candidate_json = serde_json::to_string(&value).map_err(|error| {
                        MetadataCandidateError::InvalidCandidateJson(error.to_string())
                    })?;
                    if !self
                        .database
                        .update_pending_metadata_candidate_json(item_id, &row.id, &candidate_json)
                        .await?
                    {
                        return Ok(None);
                    }
                    row.candidate_json = candidate_json;
                }
                candidate_values.push(Some(value));
            }
        }
        let items = rows
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let candidate = candidate_values.get_mut(index).and_then(Option::take);
                match candidate {
                    Some(candidate) => Ok(candidate_view_with_value(row, Some(current), candidate)),
                    None => candidate_view(row, Some(current)),
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let total = i64::try_from(items.len()).unwrap_or(i64::MAX);
        Ok(Some(MetadataCandidatePage {
            items,
            total,
            offset: 0,
            limit: 50,
        }))
    }

    fn candidate_metadata_was_fetched(candidate: &Value) -> bool {
        candidate
            .get("metadataFetched")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && candidate
                .get("metadataDetailsVersion")
                .and_then(Value::as_u64)
                .is_some_and(|version| version >= CANDIDATE_METADATA_DETAILS_VERSION)
    }

    fn merge_scraper_metadata_into_candidate(
        mut value: Value,
        details: &ScraperMetadata,
    ) -> Result<Value, MetadataCandidateError> {
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
        object.insert(
            "metadataDetailsVersion".to_owned(),
            Value::from(CANDIDATE_METADATA_DETAILS_VERSION),
        );
        Ok(value)
    }

    async fn search_child_and_store(
        &self,
        item_id: &str,
        query: &str,
        year: Option<i32>,
        scraper: &ScraperProvider,
        context: CandidateSearchContext<'_>,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        let CandidateSearchContext {
            current,
            plan,
            reuse_current_snapshot,
            automatic_match,
        } = context;
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
        let mut stored_candidates = Vec::with_capacity(parents.len());
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
                image_request.original_language = current
                    .original_language
                    .clone()
                    .or_else(|| metadata.original_language.clone());
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
            stored_candidates.push((
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
            ));
        }
        if stored_candidates.is_empty() {
            return Err(MetadataCandidateError::Scraper(ScraperError::Provider(
                last_error.unwrap_or_else(|| "scraper returned no child metadata".to_owned()),
            )));
        }
        self.store_candidates(item_id, current, stored_candidates)
            .await?;
        self.list_pending_after_search(item_id, current, reuse_current_snapshot, automatic_match)
            .await
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

    async fn store_candidates(
        &self,
        item_id: &str,
        current: &StoredMediaMetadata,
        candidates: Vec<(CandidateMetadata, Option<i64>)>,
    ) -> Result<(), MetadataCandidateError> {
        let prepared = candidates
            .into_iter()
            .map(|(candidate, expires_at)| {
                let score = candidate.score.unwrap_or_else(|| {
                    metadata_match_score(
                        &current.title,
                        current.production_year,
                        Some(&candidate.title),
                        candidate.original_title.as_deref(),
                        candidate.production_year,
                    )
                });
                let provider = candidate.provider.clone();
                let provider_id = candidate.provider_id.clone();
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
                    "metadataDetailsVersion": candidate
                        .metadata_fetched
                        .then_some(CANDIDATE_METADATA_DETAILS_VERSION),
                })
                .to_string();
                PreparedMetadataCandidate {
                    id: uuid::Uuid::now_v7().to_string(),
                    provider,
                    provider_id,
                    candidate_json,
                    score,
                    expires_at,
                }
            })
            .collect::<Vec<_>>();
        let inputs = prepared
            .iter()
            .map(|candidate| NewMetadataCandidate {
                id: &candidate.id,
                item_id,
                provider: &candidate.provider,
                provider_id: &candidate.provider_id,
                candidate_json: &candidate.candidate_json,
                score: candidate.score,
                expires_at: candidate.expires_at,
            })
            .collect::<Vec<_>>();
        self.database
            .insert_metadata_candidates(&inputs)
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
        let mut candidate_keys = Vec::with_capacity(sources.len() * SCRAPER_IMAGE_TYPES.len());
        for source in &sources {
            for image_type in SCRAPER_IMAGE_TYPES {
                candidate_keys.push((
                    image_type,
                    image_no_candidate_key(source, image_type, provider_id),
                ));
            }
        }
        let unavailable = candidate_keys
            .iter()
            .map(|(image_type, candidate_key)| MetadataImageUnavailable {
                image_type,
                candidate_key,
            })
            .collect::<Vec<_>>();
        self.database
            .mark_metadata_images_unavailable(item_id, &unavailable, now)
            .await
            .map_err(MetadataCandidateError::Storage)
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

struct PreparedMetadataCandidate {
    id: String,
    provider: String,
    provider_id: String,
    candidate_json: String,
    score: f64,
    expires_at: Option<i64>,
}

#[derive(Clone, Copy)]
enum CandidateSearchMode {
    Manual,
    AutomaticReuse,
    AutomaticFresh,
}

struct CandidateSearchOptions<'a> {
    mode: CandidateSearchMode,
    plan: MetadataRequestPlan,
    current: Option<&'a StoredMediaMetadata>,
}

struct CandidateSearchContext<'a> {
    current: &'a StoredMediaMetadata,
    plan: MetadataRequestPlan,
    reuse_current_snapshot: bool,
    automatic_match: bool,
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

fn insert_provider_id_if_missing(
    provider_ids: &mut BTreeMap<String, String>,
    provider: &str,
    provider_id: &str,
) {
    if provider_ids
        .keys()
        .any(|key| key.eq_ignore_ascii_case(provider))
    {
        return;
    }
    provider_ids.insert(provider.to_ascii_lowercase(), provider_id.to_owned());
}

fn image_attempt_identities(
    capability_identity: Option<&(String, String)>,
) -> Vec<(String, String)> {
    capability_identity
        .map(|(source, provider_id)| {
            vec![(provider_key_from_plugin_id(source), provider_id.clone())]
        })
        .unwrap_or_default()
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
        .or(current.scraper_id.as_deref())
        .map(str::trim);
    if selected_scraper
        .is_some_and(|selected| !selected.is_empty() && !scraper.matches_scraper_id(selected))
    {
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
        .take(MAX_ACTOR_DETAIL_FETCHES)
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
    preserve_identity: bool,
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
            nfo: NfoWriteService::new_with_config_dir(database.clone(), config_dir.clone()),
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

    pub(crate) async fn fill_missing_request_plan_for_current(
        &self,
        item_id: &str,
        current: &StoredMediaMetadata,
    ) -> Result<FillMissingRequestPlan, MetadataSelectionError> {
        let (_, requestable, actual_missing_image_mask) = self
            .fill_missing_request_plans_for_current(
                item_id,
                current,
                MetadataAttemptMode::RespectRetryState,
            )
            .await?;
        Ok(FillMissingRequestPlan {
            requestable,
            actual_missing_image_mask,
        })
    }

    pub(crate) async fn fallback_request_plan_for_current(
        &self,
        item_id: &str,
        current: &StoredMediaMetadata,
    ) -> Result<MetadataRequestPlan, MetadataSelectionError> {
        let (actual_plan, _, _) = self
            .fill_missing_request_plans_for_current(
                item_id,
                current,
                MetadataAttemptMode::IgnoreRetryState,
            )
            .await?;
        Ok(actual_plan)
    }

    pub(crate) async fn local_metadata_completeness_plans(
        &self,
        items: &[(&str, &StoredMediaMetadata)],
    ) -> Result<HashMap<String, Option<LocalMetadataCompletenessPlan>>, MetadataSelectionError>
    {
        let supported_items = items
            .iter()
            .filter(|(_, current)| fill_missing_fields(&current.item_type).is_some())
            .collect::<Vec<_>>();
        if supported_items.is_empty() {
            return Ok(items
                .iter()
                .map(|(item_id, _)| ((*item_id).to_owned(), None))
                .collect());
        }
        let item_ids = supported_items
            .iter()
            .map(|(item_id, _)| (*item_id).to_owned())
            .collect::<Vec<_>>();
        let strategies = self
            .database
            .list_item_media_strategy_settings_by_ids(&item_ids)
            .await?;
        let indexed_images = self.database.list_item_images_by_ids(&item_ids).await?;
        let writeback_contexts = self
            .database
            .list_media_item_writeback_contexts_by_ids(&item_ids)
            .await?;

        let mut actual_plans = Vec::with_capacity(supported_items.len());
        let mut attempt_item_ids = Vec::new();
        for (chunk_index, supported_chunk) in supported_items
            .chunks(COMPLETENESS_PLAN_ITEM_CONCURRENCY)
            .enumerate()
        {
            let chunk_start = chunk_index * COMPLETENESS_PLAN_ITEM_CONCURRENCY;
            let mut planning_tasks = Vec::with_capacity(supported_chunk.len());
            for (chunk_offset, supported_item) in supported_chunk.iter().enumerate() {
                let index = chunk_start + chunk_offset;
                let (item_id, current) = **supported_item;
                let Some((library_strategy, global_strategy)) = strategies.get(item_id) else {
                    return Err(MetadataSelectionError::ItemNotFound);
                };
                let image_policy = ImageSelectionPolicy::from_json(
                    library_strategy.as_deref(),
                    global_strategy.as_deref(),
                );
                let image_types = image_policy.enabled_types().collect::<Vec<_>>();
                let indexed_item_images = indexed_images.get(item_id).cloned().unwrap_or_default();
                let writeback_context = writeback_contexts
                    .get(item_id)
                    .cloned()
                    .ok_or(MetadataSelectionError::ItemNotFound)?;
                let item_id = item_id.to_owned();
                let current = (*current).clone();
                let service = self.clone();
                let task: CompletenessPlanTask<Result<_, MetadataSelectionError>> =
                    Box::pin(async move {
                        let local_image_types = service
                            .images
                            .local_image_types_with_indexed_images_and_context(
                                &item_id,
                                &image_types,
                                image_policy.thumbnail_scraping_mode.prefers_screenshots(),
                                &indexed_item_images,
                                &writeback_context,
                            )
                            .await?;
                        let actual_missing_image_mask =
                            missing_image_mask(&image_types, &local_image_types);
                        let (actual_plan, capability_identity) = service
                            .actual_request_plan_for_current(
                                &item_id,
                                &current,
                                image_policy,
                                actual_missing_image_mask,
                                Some(&writeback_context),
                            )
                            .await?;
                        let should_read_attempt_state = capability_identity.is_some()
                            && automatic_fill_missing_plan_has_work(actual_plan);
                        Ok((
                            item_id,
                            image_types,
                            actual_plan,
                            capability_identity,
                            should_read_attempt_state,
                        ))
                    });
                planning_tasks.push((index, task));
            }
            let chunk_plans = run_bounded_completeness_plan_tasks(planning_tasks)
                .await
                .map_err(|_| {
                    MetadataSelectionError::InvalidCandidate(
                        "local completeness planning task failed".to_owned(),
                    )
                })?
                .into_iter()
                .map(|(index, result)| result.map(|plan| (index, plan)))
                .collect::<Result<Vec<_>, _>>()?;
            actual_plans.extend(chunk_plans);
        }
        attempt_item_ids.extend(
            actual_plans
                .iter()
                .filter(|(_, (_, _, _, _, should_read_attempt_state))| *should_read_attempt_state)
                .map(|(_, (item_id, _, _, _, _))| item_id.clone()),
        );

        let attempts = self
            .database
            .list_metadata_attempts_by_item_ids(&attempt_item_ids)
            .await?;
        let mut plans = HashMap::with_capacity(items.len());
        for (
            index,
            (item_id, image_types, actual_plan, capability_identity, should_read_attempt_state),
        ) in actual_plans
        {
            let (_, current) = **supported_items
                .get(index)
                .ok_or(MetadataSelectionError::ItemNotFound)?;
            let attempt_state = should_read_attempt_state
                .then(|| attempts.get(&item_id))
                .flatten();
            let requestable_plan = requestable_metadata_plan(
                actual_plan,
                &image_types,
                capability_identity.as_ref(),
                attempt_state,
            );
            plans.insert(
                item_id.clone(),
                local_metadata_completeness_plan(&item_id, current, actual_plan, requestable_plan),
            );
        }
        for (item_id, _current) in items {
            plans.entry((*item_id).to_owned()).or_insert_with(|| None);
        }
        Ok(plans)
    }

    async fn actual_request_plan_for_current(
        &self,
        item_id: &str,
        current: &StoredMediaMetadata,
        image_policy: ImageSelectionPolicy,
        actual_missing_image_mask: u16,
        writeback_context: Option<&crate::storage::StoredMediaWritebackContext>,
    ) -> Result<(MetadataRequestPlan, Option<(String, String)>), MetadataSelectionError> {
        let details = current.nfo_metadata_json.as_deref().and_then(|value| {
            serde_json::from_str::<crate::application::nfo::LocalNfoDetails>(value).ok()
        });
        let details = if details.is_some() {
            details
        } else if let Some(context) = writeback_context {
            self.nfo
                .read_item_projection_with_writeback_context(current.season_number, context)
                .await?
                .map(|projection| projection.details)
        } else {
            self.nfo
                .read_item_projection(item_id)
                .await?
                .map(|projection| projection.details)
        };
        let credits_missing = match current.item_type.as_str() {
            "MOVIE" | "SERIES" => {
                if credits_need_actor_relation_check(details.as_ref()) {
                    !self
                        .people
                        .item_actor_relation_exists(item_id)
                        .await
                        .map_err(MetadataSelectionError::People)?
                } else {
                    true
                }
            }
            _ => false,
        };
        let mut actual_plan = metadata_request_plan(
            current,
            actual_missing_image_mask != 0,
            credits_missing,
            details.as_ref(),
        );
        actual_plan.image_policy = Some(image_policy);
        actual_plan.missing_image_mask = actual_missing_image_mask;
        if !has_selected_provider_id(current) {
            actual_plan.needs_metadata = true;
        }
        Ok((actual_plan, selected_capability_identity(current)))
    }

    pub(crate) async fn supplemental_request_plan(
        &self,
        item_id: &str,
    ) -> Result<MetadataRequestPlan, MetadataSelectionError> {
        let image_policy = self.image_selection_policy(item_id).await?;
        Ok(MetadataRequestPlan {
            image_policy: Some(image_policy),
            ..MetadataRequestPlan::full()
        })
    }

    pub(crate) fn should_schedule_thumbnail_scraper_retry(
        request_plan: FillMissingRequestPlan,
    ) -> bool {
        let Some(image_policy) = request_plan.requestable.image_policy else {
            return false;
        };
        if image_policy.thumbnail_scraping_mode != ThumbnailScrapingMode::ScraperFirst {
            return false;
        }
        let mut thumbnail_mask = 0_u16;
        for image_type in image_policy
            .enabled_types()
            .filter(|image_type| matches!(*image_type, "POSTER" | "THUMB"))
        {
            if let Some(index) = SCRAPER_IMAGE_TYPES
                .iter()
                .position(|candidate| *candidate == image_type)
            {
                thumbnail_mask |= 1_u16 << index;
            }
        }
        request_plan.actual_missing_image_mask & thumbnail_mask != 0
    }

    async fn fill_missing_request_plans_for_current(
        &self,
        item_id: &str,
        current: &StoredMediaMetadata,
        attempt_mode: MetadataAttemptMode,
    ) -> Result<(MetadataRequestPlan, MetadataRequestPlan, u16), MetadataSelectionError> {
        if fill_missing_fields(&current.item_type).is_none() {
            return Ok((MetadataRequestPlan::full(), MetadataRequestPlan::full(), 0));
        }
        let image_policy = self.image_selection_policy(item_id).await?;
        let image_types = image_policy.enabled_types().collect::<Vec<_>>();
        let local_image_types = if image_policy.thumbnail_scraping_mode.prefers_screenshots() {
            self.images
                .local_image_types_including_fallback(item_id, &image_types)
                .await?
        } else {
            self.images.local_image_types(item_id, &image_types).await?
        };
        let actual_missing_image_mask = missing_image_mask(&image_types, &local_image_types);
        let (actual_plan, selected_identity) = self
            .actual_request_plan_for_current(
                item_id,
                current,
                image_policy,
                actual_missing_image_mask,
                None,
            )
            .await?;
        let capability_identity = matches!(attempt_mode, MetadataAttemptMode::RespectRetryState)
            .then_some(selected_identity)
            .flatten();
        // Retry history only affects work that is actually missing and has a
        // confirmed provider identity. Backups use actual missing data only.
        let should_read_attempt_state =
            capability_identity.is_some() && metadata_request_plan_has_work(actual_plan);
        let attempts = if should_read_attempt_state {
            Some(self.database.list_metadata_attempts(item_id).await?)
        } else {
            None
        };
        let requestable_plan = requestable_metadata_plan(
            actual_plan,
            &image_types,
            capability_identity.as_ref(),
            attempts.as_ref(),
        );
        Ok((actual_plan, requestable_plan, actual_missing_image_mask))
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
                preserve_identity: false,
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
                preserve_identity: false,
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
                preserve_identity: false,
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
                preserve_identity: false,
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
        let mut actors = candidate_actor_credits(&candidate.candidate_json)?;
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

    pub(crate) async fn select_fallback_with_scraper_and_policy(
        &self,
        item_id: &str,
        candidate_id: &str,
        scraper_id: &str,
        image_policy: Option<ImageSelectionPolicy>,
    ) -> Result<MetadataSelectionReport, MetadataSelectionError> {
        self.select_internal(
            item_id,
            candidate_id,
            MetadataSelectionMode::FillMissing,
            MetadataSelectionOptions {
                keep_pending: false,
                scraper_id: Some(scraper_id),
                supplemental: false,
                preserve_identity: true,
                image_policy,
            },
        )
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
            payload.end_date = payload.movie_nfo.last_air_date.clone();
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
        let application_mode = if options.supplemental || options.preserve_identity {
            MetadataSelectionMode::FillMissing
        } else {
            mode
        };
        match application_mode {
            MetadataSelectionMode::FillMissing => state.apply_fill_missing(&metadata_candidate),
            MetadataSelectionMode::RefreshUnlocked => {
                state.apply_refresh_unlocked(&metadata_candidate)
            }
        }
        let mut movie_nfo = payload.movie_nfo.clone();
        movie_nfo.base = state.metadata.clone();
        let image_types = self
            .write_selected_images(
                item_id,
                &payload,
                image_policy,
                image_source,
                application_mode,
                options.supplemental,
            )
            .await?;
        let screenshot_fallback_types =
            if image_policy.thumbnail_scraping_mode.prefers_screenshots() {
                self.images
                    .fallback_image_types(item_id, &["POSTER", "THUMB"])
                    .await?
            } else {
                BTreeSet::new()
            };
        let has_primary_artwork = image_types
            .iter()
            .any(|image_type| matches!(*image_type, "POSTER" | "THUMB"))
            || !screenshot_fallback_types.is_empty()
            || self.images.has_local_image(item_id, "POSTER").await?
            || self.images.has_local_image(item_id, "THUMB").await?;
        // An empty credits response means the provider did not supply cast data;
        // avoid rewriting the persisted relation (and its database index) on
        // every fill-missing refresh. Existing local/NFO credits remain intact.
        // Full refresh keeps its replacement semantics for an explicit empty
        // credits result.
        let actor_count = if payload.actors.is_empty()
            && matches!(application_mode, MetadataSelectionMode::FillMissing)
        {
            0
        } else {
            self.people
                .persist_item_actors(item_id, &candidate.provider, &payload.actors)
                .await?
        };
        let nfo_started = std::time::Instant::now();
        let nfo_report = match current.item_type.as_str() {
            "MOVIE" => self.nfo.write_item_movie_nfo(item_id, &movie_nfo).await?,
            "SERIES" => self.nfo.write_item_series_nfo(item_id, &movie_nfo).await?,
            _ => self.nfo.write_item_nfo(item_id, &state.metadata).await?,
        };
        self.resources
            .record_metadata_stage("nfo_write", nfo_started.elapsed());
        let mut provider_ids = current_provider_ids(&current);
        if options.supplemental || options.preserve_identity {
            for (provider, provider_id) in &movie_nfo.provider_ids {
                insert_provider_id_if_missing(&mut provider_ids, provider, provider_id);
            }
            insert_provider_id_if_missing(
                &mut provider_ids,
                &candidate.provider,
                &candidate.provider_id,
            );
        } else {
            provider_ids.extend(movie_nfo.provider_ids.clone());
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
                metadata_scraper_id: (!options.supplemental
                    && !options.preserve_identity
                    && !options.keep_pending)
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
        supplemental: bool,
    ) -> Result<Vec<&'static str>, MetadataSelectionError> {
        let mut specs = Vec::new();
        let screenshot_fallback_types =
            if image_policy.thumbnail_scraping_mode.prefers_screenshots() {
                self.images
                    .fallback_image_types(item_id, &["POSTER", "THUMB"])
                    .await?
            } else {
                BTreeSet::new()
            };
        macro_rules! add_spec {
            ($image_type:expr, $urls:expr) => {
                let urls = $urls;
                if !urls.is_empty() && !screenshot_fallback_types.contains($image_type) {
                    specs.push(($image_type, urls, 0_i64));
                }
            };
        }
        if payload.typed_images_present {
            for image_type in image_policy.enabled_types() {
                if screenshot_fallback_types.contains(image_type) {
                    continue;
                }
                if let Some(urls) = payload.images.get(image_type).cloned() {
                    if image_type == "FANART" && supplemental {
                        let urls = self
                            .filter_new_supplemental_image_urls(item_id, image_type, urls)
                            .await?;
                        let start = self.images.next_image_index(item_id, image_type).await?;
                        for (offset, url) in urls.into_iter().take(MAX_IMAGE_VARIANTS).enumerate() {
                            specs.push((
                                image_type,
                                vec![url],
                                start.saturating_add(i64::try_from(offset).unwrap_or(0)),
                            ));
                        }
                    } else if image_type == "FANART" {
                        for (offset, url) in urls.into_iter().take(MAX_IMAGE_VARIANTS).enumerate() {
                            specs.push((image_type, vec![url], i64::try_from(offset).unwrap_or(0)));
                        }
                    } else {
                        add_spec!(image_type, urls);
                    }
                }
            }
        } else {
            if let Some(url) = payload.poster_url.clone() {
                add_spec!("POSTER", vec![url]);
            }
            if let Some(url) = payload.fanart_url.clone() {
                if supplemental {
                    let mut urls = self
                        .filter_new_supplemental_image_urls(item_id, "FANART", vec![url])
                        .await?;
                    if let Some(url) = urls.pop() {
                        let start = self.images.next_image_index(item_id, "FANART").await?;
                        specs.push(("FANART", vec![url], start));
                    }
                } else {
                    specs.push(("FANART", vec![url], 0));
                }
            }
        }
        let item_permits = Arc::new(Semaphore::new(IMAGE_ITEM_CONCURRENCY));
        let mut tasks = JoinSet::new();
        for (index, (image_type, urls, image_index)) in specs.into_iter().enumerate() {
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
                                .try_download_item_image_if_missing_from_scraper_at_index(
                                    &item_id,
                                    image_type,
                                    &url,
                                    &source,
                                    image_index,
                                )
                                .await
                        }
                        MetadataSelectionMode::RefreshUnlocked => {
                            if image_index == 0 {
                                images
                                    .download_item_image_from_scraper(
                                        &item_id, image_type, &url, &source,
                                    )
                                    .await
                            } else {
                                images
                                    .download_item_image_from_scraper_at_index(
                                        &item_id,
                                        image_type,
                                        &url,
                                        &source,
                                        image_index,
                                    )
                                    .await
                            }
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
        let mut result = Vec::new();
        for (_, image_type) in image_types {
            if !result.contains(&image_type) {
                result.push(image_type);
            }
        }
        Ok(result)
    }

    async fn filter_new_supplemental_image_urls(
        &self,
        item_id: &str,
        image_type: &str,
        urls: Vec<String>,
    ) -> Result<Vec<String>, MetadataSelectionError> {
        const IMAGE_SOURCE_URL_LOOKUP_BATCH_SIZE: usize = 100;

        let mut seen = HashSet::new();
        let mut filtered = Vec::with_capacity(MAX_IMAGE_VARIANTS);
        for chunk in urls.chunks(IMAGE_SOURCE_URL_LOOKUP_BATCH_SIZE) {
            let candidates = chunk
                .iter()
                .map(|url| url.trim())
                .filter(|url| !url.is_empty())
                .filter(|url| seen.insert((*url).to_owned()))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                continue;
            }
            let existing = self
                .images
                .existing_image_source_urls(item_id, image_type, &candidates)
                .await?;
            for url in candidates {
                if !existing.contains(&url) {
                    filtered.push(url);
                    if filtered.len() == MAX_IMAGE_VARIANTS {
                        return Ok(filtered);
                    }
                }
            }
        }
        Ok(filtered)
    }

    async fn image_selection_policy(
        &self,
        item_id: &str,
    ) -> Result<ImageSelectionPolicy, MetadataSelectionError> {
        let Some((library_strategy, global_strategy)) = self
            .database
            .find_item_media_strategy_settings(item_id)
            .await?
        else {
            return Err(MetadataSelectionError::ItemNotFound);
        };
        Ok(ImageSelectionPolicy::from_json(
            library_strategy.as_deref(),
            global_strategy.as_deref(),
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
        replace_if_present(&mut candidate.last_air_date, details.last_air_date.clone());
    } else {
        preserve!(rating);
        preserve!(votes);
        preserve!(tagline);
        preserve!(premiered);
        preserve!(last_air_date);
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
    for actor in existing {
        if actor.name.trim().is_empty() {
            continue;
        }
        merged.push(actor.clone());
    }
    for actor in incoming {
        if actor.name.trim().is_empty() {
            continue;
        }
        if let Some(stored) = merged.iter_mut().find(|stored| same_actor(stored, actor)) {
            merge_missing_actor_fields(stored, actor);
        } else {
            merged.push(actor.clone());
        }
    }
    merged
}

fn same_actor(left: &ActorCredit, right: &ActorCredit) -> bool {
    if !left.id.trim().is_empty() && !right.id.trim().is_empty() {
        left.provider
            .as_deref()
            .unwrap_or_default()
            .eq_ignore_ascii_case(right.provider.as_deref().unwrap_or_default())
            && left.id.eq_ignore_ascii_case(&right.id)
    } else {
        left.name.trim().eq_ignore_ascii_case(right.name.trim())
            && left
                .character
                .as_deref()
                .unwrap_or_default()
                .eq_ignore_ascii_case(right.character.as_deref().unwrap_or_default())
    }
}

fn merge_missing_actor_fields(target: &mut ActorCredit, incoming: &ActorCredit) {
    if target.provider.is_none() {
        target.provider = incoming.provider.clone();
    }
    fill_missing_string(&mut target.character, &incoming.character);
    if target.order.is_none() {
        target.order = incoming.order;
    }
    fill_missing_string(&mut target.profile_url, &incoming.profile_url);
    for identity in &incoming.identities {
        if !target.identities.iter().any(|stored| {
            stored.provider.eq_ignore_ascii_case(&identity.provider)
                && stored.id.eq_ignore_ascii_case(&identity.id)
        }) {
            target.identities.push(identity.clone());
        }
    }
    match (&mut target.person, &incoming.person) {
        (Some(target), Some(incoming)) => merge_missing_person_fields(target, incoming),
        (None, Some(incoming)) => target.person = Some(incoming.clone()),
        _ => {}
    }
}

fn fill_missing_string(target: &mut Option<String>, incoming: &Option<String>) {
    if target
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
        && incoming
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        *target = incoming.clone();
    }
}

fn merge_missing_person_fields(
    target: &mut crate::application::people::PersonMetadata,
    incoming: &crate::application::people::PersonMetadata,
) {
    fill_missing_string(&mut target.biography, &incoming.biography);
    fill_missing_string(&mut target.birthday, &incoming.birthday);
    fill_missing_string(&mut target.deathday, &incoming.deathday);
    fill_missing_string(
        &mut target.known_for_department,
        &incoming.known_for_department,
    );
    fill_missing_string(&mut target.place_of_birth, &incoming.place_of_birth);
    if target.production_year.is_none() {
        target.production_year = incoming.production_year;
    }
    fill_missing_string(&mut target.premiere_date, &incoming.premiere_date);
    for (provider, id) in &incoming.provider_ids {
        target
            .provider_ids
            .entry(provider.clone())
            .or_insert_with(|| id.clone());
    }
    target.genres = merge_string_values(&target.genres, &incoming.genres);
    target.tags = merge_string_values(&target.tags, &incoming.tags);
    target.production_locations =
        merge_string_values(&target.production_locations, &incoming.production_locations);
    target.taglines = merge_string_values(&target.taglines, &incoming.taglines);
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

pub(crate) fn has_selected_provider_id(current: &StoredMediaMetadata) -> bool {
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
    thumbnail_scraping_mode: ThumbnailScrapingMode,
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
            (
                self.poster && !matches!(self.thumbnail_scraping_mode, ThumbnailScrapingMode::None),
                "POSTER",
            ),
            (true, "FANART"),
            (self.logo, "LOGO"),
            (
                self.thumbnail
                    && !matches!(self.thumbnail_scraping_mode, ThumbnailScrapingMode::None),
                "THUMB",
            ),
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
    #[serde(default)]
    thumbnail_scraping_mode: Option<String>,
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
        thumbnail_scraping_mode: ThumbnailScrapingMode::ScraperFirst,
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
        thumbnail_scraping_mode: ThumbnailScrapingMode::parse(
            strategy.images.thumbnail_scraping_mode.as_deref(),
        ),
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
        last_air_date: end_date.clone(),
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

#[derive(Deserialize)]
struct CandidateActorCredits {
    #[serde(default)]
    actors: Vec<ActorCredit>,
}

fn candidate_actor_credits(
    candidate_json: &str,
) -> Result<Vec<ActorCredit>, MetadataSelectionError> {
    let candidate = serde_json::from_str::<CandidateActorCredits>(candidate_json)
        .map_err(|error| MetadataSelectionError::InvalidCandidate(error.to_string()))?;
    validate_candidate_actors(candidate.actors)
}

fn candidate_actors(value: &Value) -> Result<Vec<ActorCredit>, MetadataSelectionError> {
    let Some(raw) = value.get("actors") else {
        return Ok(Vec::new());
    };
    let actors = raw.as_array().ok_or_else(|| {
        MetadataSelectionError::InvalidCandidate("actors must be an array".to_owned())
    })?;
    let actors = actors
        .iter()
        .take(MAX_MOVIE_NFO_ACTORS)
        .map(|actor| {
            serde_json::from_value::<ActorCredit>(actor.clone()).map_err(|error| {
                MetadataSelectionError::InvalidCandidate(format!("actor is invalid: {error}"))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_candidate_actors(actors)
}

fn validate_candidate_actors(
    actors: Vec<ActorCredit>,
) -> Result<Vec<ActorCredit>, MetadataSelectionError> {
    actors
        .into_iter()
        .take(MAX_MOVIE_NFO_ACTORS)
        .map(|actor| {
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
    Ok(candidate_view_with_value(row, current, candidate))
}

fn candidate_view_with_value(
    row: StoredMetadataCandidate,
    current: Option<&StoredMediaMetadata>,
    mut candidate: Value,
) -> MetadataCandidateView {
    if let Some(candidate) = candidate.as_object_mut() {
        candidate.remove("metadataDetailsVersion");
    }
    let field_diffs = current
        .map(|current| field_diffs(current, &candidate))
        .unwrap_or_default();
    MetadataCandidateView {
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
    }
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
        ACTOR_METADATA_FETCH_CONCURRENCY, COMPLETENESS_PLAN_ITEM_CONCURRENCY,
        FillMissingRequestPlan, ImageSelectionPolicy, MAX_ACTOR_DETAIL_FETCHES,
        MetadataCandidateService, MetadataRequestPlan, MetadataSelectionService,
        SCRAPER_IMAGE_TYPES, candidate_actor_credits, candidate_actors, capability_needs_request,
        completeness_capabilities, credits_need_actor_relation_check,
        default_image_selection_policy, enrich_actor_metadata, generic_candidate_actors,
        generic_candidate_images, image_attempt_identities, local_metadata_completeness_plan,
        merge_actor_values, merge_supplemental_movie_nfo, metadata_completeness_fingerprint,
        metadata_match_score, metadata_request_plan, metadata_request_plan_has_work,
        parse_image_selection_policy, run_bounded_completeness_plan_tasks,
        selected_scraper_provider_id,
    };
    use crate::application::scraper::{
        ScraperActorCredit, ScraperAdapter, ScraperCreditsResponse, ScraperError,
        ScraperExternalIdsResponse, ScraperFuture, ScraperGetRequest, ScraperImage,
        ScraperImageRequest, ScraperImagesResponse, ScraperItemType, ScraperMetadata,
        ScraperMetadataBundle, ScraperProvider, ScraperSearchRequest, ScraperSearchResponse,
        ScraperTrailersResponse,
    };
    use crate::application::thumbnail_policy::ThumbnailScrapingMode;
    use crate::storage::{StoredMediaMetadata, StoredMetadataCapabilityAttempt};
    use serde_json::json;
    use std::collections::BTreeMap;
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

    #[tokio::test]
    async fn completeness_plan_page_tasks_are_bounded_and_keep_input_order() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let tasks = (0..12)
            .map(|index| {
                let active = Arc::clone(&active);
                let maximum = Arc::clone(&maximum);
                (
                    index,
                    Box::pin(async move {
                        let active_count = active.fetch_add(1, Ordering::SeqCst) + 1;
                        maximum.fetch_max(active_count, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        index
                    }) as super::CompletenessPlanTask<usize>,
                )
            })
            .collect();

        let results = run_bounded_completeness_plan_tasks(tasks)
            .await
            .expect("bounded completeness planning tasks should join");

        assert_eq!(
            results.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            (0..12).collect::<Vec<_>>()
        );
        assert_eq!(
            results.iter().map(|(_, value)| *value).collect::<Vec<_>>(),
            (0..12).collect::<Vec<_>>()
        );
        assert!(maximum.load(Ordering::SeqCst) > 1);
        assert!(maximum.load(Ordering::SeqCst) <= COMPLETENESS_PLAN_ITEM_CONCURRENCY);

        let error_tasks = (0..3)
            .map(|index| {
                (
                    index,
                    Box::pin(async move {
                        tokio::time::sleep(Duration::from_millis(if index == 0 { 20 } else { 1 }))
                            .await;
                        Err::<usize, _>(if index == 0 {
                            "first input error"
                        } else {
                            "later error"
                        })
                    })
                        as super::CompletenessPlanTask<Result<usize, &'static str>>,
                )
            })
            .collect();
        let error_results = run_bounded_completeness_plan_tasks(error_tasks)
            .await
            .expect("bounded error tasks should join");
        let first_error = error_results
            .into_iter()
            .map(|(_, result)| result)
            .collect::<Result<Vec<_>, _>>()
            .expect_err("input-order error should be preserved");
        assert_eq!(first_error, "first input error");
    }

    #[tokio::test]
    async fn actor_metadata_enrichment_keeps_the_cast_limit_separate_from_detail_fetches() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let scraper = ScraperProvider::from_adapter(DelayedActorAdapter { active, maximum });
        let mut actors = (0..100)
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

        assert!(
            actors
                .iter()
                .take(MAX_ACTOR_DETAIL_FETCHES)
                .all(|actor| actor.person.is_some())
        );
        assert!(
            actors
                .iter()
                .skip(MAX_ACTOR_DETAIL_FETCHES)
                .all(|actor| actor.person.is_none())
        );
    }

    #[test]
    fn actor_enrichment_decodes_only_the_candidate_cast() {
        let actors = candidate_actor_credits(
            r#"{"actors":[{"id":"9","name":" Actor ","character":"Role","order":2}],"images":{"FANART":["https://example.test/backdrop.jpg"]},"largeUnusedField":{"value":"ignored"}}"#,
        )
        .expect("decode candidate cast");

        assert_eq!(actors.len(), 1);
        assert_eq!(actors[0].id, "9");
        assert_eq!(actors[0].name, "Actor");
        assert_eq!(actors[0].character.as_deref(), Some("Role"));
        assert_eq!(actors[0].order, Some(2));
    }

    #[test]
    fn cached_candidate_value_is_merged_without_reparsing_its_json() {
        let candidate = json!({
            "title": "Example",
            "providerIds": {"tmdb": "7"},
            "unknownField": {"preserved": true},
            "metadataFetched": false
        });
        assert!(!MetadataCandidateService::candidate_metadata_was_fetched(
            &candidate
        ));

        let merged = MetadataCandidateService::merge_scraper_metadata_into_candidate(
            candidate,
            &ScraperMetadata {
                overview: Some("Fetched summary".to_owned()),
                provider_ids: BTreeMap::from([("imdb".to_owned(), "tt7".to_owned())]),
                ..ScraperMetadata::default()
            },
        )
        .expect("merge cached candidate value");

        assert_eq!(merged["overview"], "Fetched summary");
        assert_eq!(merged["providerIds"]["tmdb"], "7");
        assert_eq!(merged["providerIds"]["imdb"], "tt7");
        assert_eq!(merged["unknownField"]["preserved"], true);
        assert!(MetadataCandidateService::candidate_metadata_was_fetched(
            &merged
        ));
    }

    #[test]
    fn metadata_refresh_keeps_backdrop_images_as_fanart() {
        let enabled_types = default_image_selection_policy()
            .enabled_types()
            .collect::<Vec<_>>();

        assert!(enabled_types.contains(&"FANART"));
    }

    #[test]
    fn none_thumbnail_mode_removes_only_poster_and_thumb_from_scraper_types() {
        let policy = parse_image_selection_policy(
            r#"{"images":{"thumbnailScrapingMode":"NONE","poster":true,"thumbnail":true,"logo":true}}"#,
        )
        .expect("strategy JSON is valid");
        let enabled_types = policy.enabled_types().collect::<Vec<_>>();

        assert!(!enabled_types.contains(&"POSTER"));
        assert!(!enabled_types.contains(&"THUMB"));
        assert!(enabled_types.contains(&"FANART"));
        assert!(enabled_types.contains(&"LOGO"));
    }

    #[test]
    fn thumbnail_retry_uses_actual_missing_images_from_the_request_plan() {
        let mut policy = default_image_selection_policy();
        policy.thumbnail_scraping_mode = ThumbnailScrapingMode::ScraperFirst;
        let request_plan = FillMissingRequestPlan {
            requestable: MetadataRequestPlan {
                image_policy: Some(policy),
                missing_image_mask: 0,
                ..MetadataRequestPlan::default()
            },
            actual_missing_image_mask: 1,
        };
        assert!(MetadataSelectionService::should_schedule_thumbnail_scraper_retry(request_plan));

        assert!(
            !MetadataSelectionService::should_schedule_thumbnail_scraper_retry(
                FillMissingRequestPlan {
                    actual_missing_image_mask: 0,
                    ..request_plan
                }
            )
        );
        assert!(
            !MetadataSelectionService::should_schedule_thumbnail_scraper_retry(
                FillMissingRequestPlan {
                    requestable: MetadataRequestPlan {
                        image_policy: Some(ImageSelectionPolicy {
                            thumbnail_scraping_mode: ThumbnailScrapingMode::ScreenshotFirst,
                            ..policy
                        }),
                        ..request_plan.requestable
                    },
                    ..request_plan
                }
            )
        );
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
        let mut current = StoredMediaMetadata {
            library_id: "library".to_owned(),
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
            identification_status: "ONLINE_CONFIRMED".to_owned(),
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
            metadata_fingerprint: None,
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
        // Existing items are not re-scraped solely because their local NFO lacks
        // optional rich fields. A user-triggered full refresh still uses the full plan.
        let explicit_full_plan = MetadataRequestPlan::full();
        assert!(explicit_full_plan.needs_metadata);
        assert!(explicit_full_plan.needs_external_ids);
        assert!(explicit_full_plan.needs_trailers);

        let complete_provider_ids = current.provider_ids_json.clone();
        current.provider_ids_json = Some(json!({"tmdb": "1"}).to_string());
        let optional_details = crate::application::nfo::LocalNfoDetails {
            directors: vec![crate::application::nfo::LocalNfoCredit {
                provider_id: "director-1".to_owned(),
                name: "Director".to_owned(),
            }],
            writers: vec![crate::application::nfo::LocalNfoCredit {
                provider_id: "writer-1".to_owned(),
                name: "Writer".to_owned(),
            }],
            ..crate::application::nfo::LocalNfoDetails::default()
        };
        assert!(optional_details.tagline.is_none());
        assert!(optional_details.website.is_none());
        assert!(optional_details.certification.is_none());
        assert!(optional_details.countries.is_empty());
        assert!(optional_details.genres.is_empty());
        assert!(optional_details.studios.is_empty());
        let optional_details_plan =
            metadata_request_plan(&current, false, false, Some(&optional_details));
        assert!(!optional_details_plan.needs_metadata);
        assert!(optional_details_plan.needs_external_ids);
        assert!(optional_details_plan.needs_trailers);
        assert!(metadata_request_plan_has_work(optional_details_plan));
        let optional_details_completeness = local_metadata_completeness_plan(
            "movie-1",
            &current,
            optional_details_plan,
            optional_details_plan,
        )
        .expect("movie completeness plan is supported");
        assert!(
            !optional_details_completeness.has_requestable_capability,
            "optional trailers and external IDs must not alone enqueue automatic FILL_MISSING"
        );
        assert!(
            optional_details_completeness
                .capabilities
                .contains(&("EXTERNAL_IDS".to_owned(), true))
        );
        assert!(
            optional_details_completeness
                .capabilities
                .contains(&("TRAILERS".to_owned(), true))
        );
        let required_work_completeness = local_metadata_completeness_plan(
            "movie-1",
            &current,
            MetadataRequestPlan {
                needs_credits: true,
                ..optional_details_plan
            },
            MetadataRequestPlan {
                needs_credits: true,
                ..optional_details_plan
            },
        )
        .expect("movie completeness plan is supported");
        assert!(
            required_work_completeness.has_requestable_capability,
            "optional provider extras may accompany required automatic fill work"
        );
        current.provider_ids_json = complete_provider_ids;

        let poster_index = SCRAPER_IMAGE_TYPES
            .iter()
            .position(|image_type| *image_type == "POSTER")
            .expect("poster capability index");
        let fanart_index = SCRAPER_IMAGE_TYPES
            .iter()
            .position(|image_type| *image_type == "FANART")
            .expect("fanart capability index");
        let mut actual_plan = plan;
        actual_plan.image_policy = Some(default_image_selection_policy());
        actual_plan.missing_image_mask = (1_u16 << poster_index) | (1_u16 << fanart_index);
        let mut requestable_plan = actual_plan;
        requestable_plan.missing_image_mask = 1_u16 << poster_index;
        requestable_plan.needs_images = true;

        let completeness =
            local_metadata_completeness_plan("movie-1", &current, actual_plan, requestable_plan)
                .expect("movie completeness plan is supported");
        assert!(completeness.has_requestable_capability);
        assert_eq!(completeness.input_fingerprint.len(), 32);
        assert!(
            completeness
                .capabilities
                .contains(&("POSTER".to_owned(), true))
        );
        assert!(
            completeness
                .capabilities
                .contains(&("FANART".to_owned(), true))
        );
        assert!(
            completeness
                .capabilities
                .contains(&("METADATA".to_owned(), false))
        );
        assert_eq!(
            completeness.input_fingerprint,
            metadata_completeness_fingerprint("movie-1", &current, actual_plan, requestable_plan,)
        );

        let mut poster_disabled_policy = default_image_selection_policy();
        poster_disabled_policy.poster = false;
        let mut poster_disabled_plan = actual_plan;
        poster_disabled_plan.image_policy = Some(poster_disabled_policy);
        let disabled_capabilities = completeness_capabilities(&current, poster_disabled_plan);
        assert!(
            !disabled_capabilities
                .iter()
                .any(|(capability, _)| capability == "POSTER")
        );
        assert!(disabled_capabilities.contains(&("FANART".to_owned(), true)));
        assert_ne!(
            metadata_completeness_fingerprint("movie-1", &current, actual_plan, requestable_plan,),
            metadata_completeness_fingerprint(
                "movie-1",
                &current,
                poster_disabled_plan,
                requestable_plan,
            ),
            "an image policy change updates the input fingerprint"
        );

        let no_requestable_image_plan = local_metadata_completeness_plan(
            "movie-1",
            &current,
            actual_plan,
            MetadataRequestPlan {
                needs_metadata: false,
                needs_images: false,
                missing_image_mask: 0,
                ..actual_plan
            },
        )
        .expect("movie completeness plan is supported");
        assert!(
            no_requestable_image_plan
                .capabilities
                .contains(&("POSTER".to_owned(), true))
        );
        assert!(!no_requestable_image_plan.has_requestable_capability);
        let unavailable_attempts = [StoredMetadataCapabilityAttempt {
            provider: "tmdb".to_owned(),
            provider_id: "1".to_owned(),
            capability: "CREDITS".to_owned(),
            status: "UNAVAILABLE".to_owned(),
            next_retry_at: None,
        }];
        let identity = ("tmdb".to_owned(), "1".to_owned());
        assert!(!capability_needs_request(
            &unavailable_attempts,
            Some(&identity),
            "CREDITS"
        ));
        assert!(capability_needs_request(
            &unavailable_attempts,
            Some(&identity),
            "TRAILERS"
        ));
        assert!(
            capability_needs_request(&unavailable_attempts, None, "CREDITS"),
            "attempt history cannot apply until a provider identity is selected"
        );
        assert!(
            image_attempt_identities(None).is_empty(),
            "image attempt history cannot apply until a provider identity is selected"
        );

        let first_fingerprint = completeness.input_fingerprint;
        current.overview = Some("Updated local overview".to_owned());
        assert_eq!(
            first_fingerprint,
            metadata_completeness_fingerprint("movie-1", &current, actual_plan, requestable_plan,),
            "unrelated metadata changes must not requeue every missing capability"
        );
        current.provider_ids_json = Some(serde_json::json!({"tmdb": "changed"}).to_string());
        assert_ne!(
            first_fingerprint,
            metadata_completeness_fingerprint("movie-1", &current, actual_plan, requestable_plan,),
            "provider identity changes must update the input fingerprint"
        );
        current.item_type = "VIDEO".to_owned();
        assert!(
            local_metadata_completeness_plan("movie-1", &current, actual_plan, requestable_plan,)
                .is_none(),
            "unsupported video types do not create online completeness plans"
        );
    }

    #[test]
    fn old_candidate_metadata_cache_is_not_treated_as_complete_details() {
        let missing_version =
            serde_json::from_str::<serde_json::Value>(r#"{"metadataFetched":true}"#)
                .expect("valid candidate JSON");
        let not_fetched = serde_json::from_str::<serde_json::Value>(
            r#"{"metadataFetched":false,"metadataDetailsVersion":2}"#,
        )
        .expect("valid candidate JSON");
        let current_details = serde_json::from_str::<serde_json::Value>(
            r#"{"metadataFetched":true,"metadataDetailsVersion":2}"#,
        )
        .expect("valid candidate JSON");
        assert!(!MetadataCandidateService::candidate_metadata_was_fetched(
            &missing_version
        ));
        assert!(!MetadataCandidateService::candidate_metadata_was_fetched(
            &not_fetched
        ));
        assert!(MetadataCandidateService::candidate_metadata_was_fetched(
            &current_details
        ));
    }

    #[test]
    fn fill_missing_reuses_provider_id_when_item_scraper_is_not_persisted() {
        let current = StoredMediaMetadata {
            library_id: "library".to_owned(),
            item_type: "MOVIE".to_owned(),
            title: "Example Movie".to_owned(),
            original_title: None,
            overview: None,
            production_year: Some(2020),
            premiere_date: None,
            last_air_date: None,
            status: None,
            original_language: None,
            rating: None,
            provider_ids_json: Some(json!({"tmdb": "603"}).to_string()),
            metadata_scraper_id: None,
            identification_status: "LOCAL_CONFIRMED".to_owned(),
            scraper_id: None,
            provenance_json: None,
            locked_fields_json: None,
            nfo_metadata_json: None,
            metadata_fingerprint: None,
            series_item_id: None,
            series_title: None,
            series_production_year: None,
            series_provider_name: None,
            series_provider_id: None,
            season_number: None,
            episode_number: None,
        };
        let scraper = ScraperProvider::from_adapter(DelayedActorAdapter {
            active: Arc::new(AtomicUsize::new(0)),
            maximum: Arc::new(AtomicUsize::new(0)),
        });

        assert_eq!(
            selected_scraper_provider_id(&current, &scraper).as_deref(),
            Some("603")
        );
    }

    #[test]
    fn locked_missing_fields_do_not_keep_metadata_requests_pending() {
        let current = StoredMediaMetadata {
            library_id: "library".to_owned(),
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
            identification_status: "ONLINE_CONFIRMED".to_owned(),
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
            metadata_fingerprint: None,
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
        let plan = metadata_request_plan(&current, false, false, Some(&details));
        assert!(!plan.needs_metadata);
    }

    #[test]
    fn credits_relation_check_is_needed_only_when_local_crew_details_are_complete() {
        let missing_director = crate::application::nfo::LocalNfoDetails {
            directors: vec![crate::application::nfo::LocalNfoCredit {
                provider_id: "director-1".to_owned(),
                name: "Director".to_owned(),
            }],
            ..crate::application::nfo::LocalNfoDetails::default()
        };
        let complete_crew = crate::application::nfo::LocalNfoDetails {
            directors: vec![crate::application::nfo::LocalNfoCredit {
                provider_id: "director-1".to_owned(),
                name: "Director".to_owned(),
            }],
            writers: vec![crate::application::nfo::LocalNfoCredit {
                provider_id: "writer-1".to_owned(),
                name: "Writer".to_owned(),
            }],
            ..crate::application::nfo::LocalNfoDetails::default()
        };

        assert!(!credits_need_actor_relation_check(None));
        assert!(!credits_need_actor_relation_check(Some(&missing_director)));
        assert!(credits_need_actor_relation_check(Some(&complete_crew)));
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
    fn movie_candidate_cast_is_bounded_to_100_and_keeps_provider_order() {
        let credits = (0..105)
            .map(|order| ScraperActorCredit {
                provider_id: order.to_string(),
                name: Some(format!("Actor {order}")),
                character: Some(format!("Character {order}")),
                order: Some(order),
                profile_url: None,
            })
            .collect::<Vec<_>>();

        let actors = generic_candidate_actors(&credits);

        assert_eq!(actors.len(), 100);
        assert_eq!(actors[0].name, "Actor 0");
        assert_eq!(actors[99].name, "Actor 99");
        assert_eq!(actors[99].order, Some(99));

        let candidate_values = (0..105)
            .map(|order| {
                json!({
                    "id": format!("person-{order}"),
                    "provider": "tmdb",
                    "name": format!("Actor {order}"),
                    "character": format!("Character {order}"),
                    "order": order
                })
            })
            .collect::<Vec<_>>();
        let parsed = candidate_actors(&json!({ "actors": candidate_values }))
            .expect("bounded candidate cast should parse");
        assert_eq!(parsed.len(), 100);
        assert_eq!(parsed[99].name, "Actor 99");
        assert_eq!(parsed[99].order, Some(99));
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

    #[test]
    fn supplemental_actor_merge_fills_missing_fields_without_duplicate() {
        let existing = [super::ActorCredit {
            id: "actor-1".to_owned(),
            provider: Some("main".to_owned()),
            identities: Vec::new(),
            name: "主演员".to_owned(),
            character: None,
            order: Some(0),
            profile_url: None,
            person: None,
        }];
        let incoming = [super::ActorCredit {
            id: "actor-1".to_owned(),
            provider: Some("main".to_owned()),
            identities: Vec::new(),
            name: "补充来源演员名".to_owned(),
            character: Some("补充角色".to_owned()),
            order: Some(4),
            profile_url: Some("https://images.example/actor-1.jpg".to_owned()),
            person: Some(crate::application::people::PersonMetadata {
                biography: Some("补充人物简介".to_owned()),
                ..Default::default()
            }),
        }];

        let merged = merge_actor_values(&existing, &incoming);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].name, "主演员");
        assert_eq!(merged[0].character.as_deref(), Some("补充角色"));
        assert_eq!(merged[0].order, Some(0));
        assert_eq!(
            merged[0].profile_url.as_deref(),
            Some("https://images.example/actor-1.jpg")
        );
        assert_eq!(
            merged[0]
                .person
                .as_ref()
                .and_then(|person| person.biography.as_deref()),
            Some("补充人物简介")
        );
    }
}
