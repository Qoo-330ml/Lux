use std::{
    collections::BTreeMap,
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    application::{
        images::{ImageWriteError, ImageWriteService},
        media_matching::{MediaKind, parse_media_name, title_candidates},
        metadata::{MetadataCandidate, MetadataField, MetadataSource, MetadataState, NfoMetadata},
        nfo::{NfoWriteError, NfoWriteService},
        people::{ActorCredit, PeopleError},
        scraper::{
            ScraperError, ScraperGetRequest, ScraperImageRequest, ScraperItemType, ScraperMetadata,
        },
        tmdb::TmdbError,
        tmdb_plugin::TmdbProvider,
    },
    storage::{
        Database, NewMetadataCandidate, SelectedMetadataUpdate, StorageError, StoredMediaMetadata,
        StoredMetadataCandidate,
    },
};

#[cfg(test)]
use crate::application::tmdb::TmdbCastMember;

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
        tmdb: &TmdbProvider,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        let current = self
            .database
            .find_media_item_metadata(item_id)
            .await?
            .ok_or(MetadataCandidateError::ItemNotFound)?;
        let kind = match current.item_type.as_str() {
            "MOVIE" => MediaKind::Movie,
            "SERIES" => MediaKind::Series,
            "SEASON" | "EPISODE" => {
                return self
                    .search_child_and_store(item_id, query, year, &current, tmdb)
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
        let response = search_generic(tmdb, item_type, query, year)
            .await
            .map_err(MetadataCandidateError::Scraper)?;
        let expires_at = candidate_expiry();
        for result in response.items.into_iter().take(20) {
            let Some((provider, provider_id)) = tmdb.selected_provider_entry(&result) else {
                continue;
            };
            let provider = provider.to_owned();
            let provider_id = provider_id.to_owned();
            let details = if matches!(
                item_type,
                crate::application::scraper::ScraperItemType::Series
            ) {
                tmdb.get_generic(crate::application::scraper::ScraperGetRequest::new(
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
            let images = tmdb
                .images_generic(crate::application::scraper::ScraperImageRequest::new(
                    item_type,
                    provider_id.clone(),
                    "zh-CN",
                ))
                .await
                .unwrap_or_default();
            let actors = if matches!(
                item_type,
                crate::application::scraper::ScraperItemType::Movie
                    | crate::application::scraper::ScraperItemType::Series
            ) {
                tmdb.credits_generic(crate::application::scraper::ScraperGetRequest::new(
                    item_type,
                    provider_id.clone(),
                    "zh-CN",
                ))
                .await
                .map(|credits| generic_candidate_actors(&credits.cast))
                .unwrap_or_default()
            } else {
                Vec::new()
            };
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
                    release_date: details
                        .as_ref()
                        .and_then(|value| value.premiere_date.clone())
                        .or(result.premiere_date),
                    end_date: details.as_ref().and_then(|value| value.end_date.clone()),
                    status: details.as_ref().and_then(|value| value.status.clone()),
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
                    provider,
                    provider_id,
                    images: generic_candidate_images(&images.images, item_type),
                    actors,
                    score: None,
                },
                expires_at,
            )
            .await?;
        }
        self.list_for_item(item_id, None, 0, 50).await
    }

    async fn search_child_and_store(
        &self,
        item_id: &str,
        query: &str,
        year: Option<i32>,
        current: &StoredMediaMetadata,
        scraper: &TmdbProvider,
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
            let metadata = match scraper.get_generic(request).await {
                Ok(metadata) => metadata,
                Err(error) => {
                    last_error = Some(error.to_string());
                    continue;
                }
            };
            let Some(provider_id) = selected_metadata_provider_id(&metadata, &parent.provider)
            else {
                continue;
            };
            let mut image_request =
                ScraperImageRequest::new(item_type, &parent.provider_id, "zh-CN");
            image_request.season_number = Some(season_number);
            image_request.episode_number = episode_number;
            let images = scraper
                .images_generic(image_request)
                .await
                .unwrap_or_default();
            let title = metadata
                .title
                .clone()
                .or_else(|| metadata.original_title.clone())
                .unwrap_or_else(|| current.title.clone());
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
                    provider: parent.provider,
                    provider_id,
                    images: generic_candidate_images(&images.images, item_type),
                    actors: Vec::new(),
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
        scraper: &TmdbProvider,
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
                        result.title.as_deref().or(result.original_title.as_deref()),
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
                candidate.production_year,
            )
        });
        let candidate_json = json!({
            "title": candidate.title,
            "originalTitle": candidate.original_title,
            "overview": candidate.overview,
            "releaseDate": candidate.release_date,
            "premiereDate": candidate.release_date,
            "endDate": candidate.end_date,
            "status": candidate.status,
            "productionYear": candidate.production_year,
            "rating": candidate.rating,
            "originalLanguage": candidate.original_language,
            "images": candidate.images,
            "actors": candidate.actors,
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
}

struct CandidateMetadata {
    title: String,
    original_title: Option<String>,
    overview: Option<String>,
    release_date: Option<String>,
    end_date: Option<String>,
    status: Option<String>,
    production_year: Option<i32>,
    rating: Option<f64>,
    original_language: Option<String>,
    provider: String,
    provider_id: String,
    images: BTreeMap<String, Vec<String>>,
    actors: Vec<ActorCredit>,
    score: Option<f64>,
}

struct ParentProvider {
    provider: String,
    provider_id: String,
    score: f64,
}

async fn search_generic(
    scraper: &TmdbProvider,
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

fn metadata_match_score(
    current_title: &str,
    current_year: Option<i64>,
    candidate_title: Option<&str>,
    candidate_year: Option<i32>,
) -> f64 {
    let title_score = candidate_title
        .map(|candidate_title| {
            let current_normalized =
                crate::application::media_matching::normalize_title(current_title);
            let candidate_normalized =
                crate::application::media_matching::normalize_title(candidate_title);
            if current_normalized == candidate_normalized {
                80.0
            } else if candidate_normalized.contains(&current_normalized)
                || current_normalized.contains(&candidate_normalized)
            {
                50.0
            } else {
                0.0
            }
        })
        .unwrap_or(0.0);
    if title_score < 80.0
        && current_year
            .and_then(|value| i32::try_from(value).ok())
            .is_some_and(|year| Some(year) == candidate_year)
    {
        title_score + 20.0
    } else {
        title_score
    }
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
    Tmdb(TmdbError),
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
            Self::Tmdb(error) => error.fmt(formatter),
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
        .take(12)
        .filter_map(|member| {
            let id = member.provider_id.trim();
            if id.is_empty()
                || id.len() > 128
                || !id.chars().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
                })
            {
                return None;
            }
            let name = member.name.as_deref()?.trim();
            (!name.is_empty()).then(|| ActorCredit {
                id: id.to_owned(),
                name: name.to_owned(),
                character: member
                    .character
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
                order: member.order,
                profile_url: member.profile_url.clone(),
            })
        })
        .collect()
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

#[derive(Clone)]
pub struct MetadataSelectionService {
    database: Database,
    nfo: NfoWriteService,
    images: ImageWriteService,
    people: crate::application::people::PeopleService,
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
            database,
            images,
            people: crate::application::people::PeopleService::new(config_dir),
        }
    }

    pub(crate) async fn is_fill_missing_complete(
        &self,
        item_id: &str,
    ) -> Result<bool, MetadataSelectionError> {
        let current = self
            .database
            .find_media_item_metadata(item_id)
            .await?
            .ok_or(MetadataSelectionError::ItemNotFound)?;
        let Some(fields) = fill_missing_fields(&current.item_type) else {
            return Ok(false);
        };
        let state = metadata_state(&current);
        if !state.has_complete_fill_values(fields)
            || !fill_missing_scalar_values_complete(&current)
            || !has_selected_provider_id(&current)
        {
            return Ok(false);
        }
        let image_policy = self.image_selection_policy(item_id).await?;
        for image_type in image_policy.enabled_types() {
            if !self.images.has_local_image(item_id, image_type).await? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub async fn select(
        &self,
        item_id: &str,
        candidate_id: &str,
        mode: MetadataSelectionMode,
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
        let payload = candidate_payload(&candidate)?;
        let image_policy = self.image_selection_policy(item_id).await?;
        let mut state = metadata_state(&current);
        let metadata_candidate = MetadataCandidate {
            source: MetadataSource::ScraperLocalized,
            metadata: payload.metadata,
        };
        match mode {
            MetadataSelectionMode::FillMissing => state.apply_fill_missing(&metadata_candidate),
            MetadataSelectionMode::RefreshUnlocked => {
                state.apply_refresh_unlocked(&metadata_candidate)
            }
        }
        let mut image_types = Vec::new();
        if payload.typed_images_present {
            for image_type in image_policy.enabled_types() {
                let Some(url) = payload.images.get(image_type).and_then(|urls| urls.first()) else {
                    continue;
                };
                if self
                    .write_selected_image(item_id, image_type, url, &candidate.provider, mode)
                    .await?
                    .is_some()
                {
                    image_types.push(image_type);
                }
            }
        } else {
            if let Some(url) = payload.poster_url.as_deref() {
                if self
                    .write_selected_image(item_id, "POSTER", url, &candidate.provider, mode)
                    .await?
                    .is_some()
                {
                    image_types.push("POSTER");
                }
            }
            if let Some(url) = payload.fanart_url.as_deref() {
                if self
                    .write_selected_image(item_id, "FANART", url, &candidate.provider, mode)
                    .await?
                    .is_some()
                {
                    image_types.push("FANART");
                }
            }
        }
        let actor_count = self
            .people
            .persist_item_actors(item_id, &candidate.provider, &payload.actors)
            .await?;
        let nfo_report = self.nfo.write_item_nfo(item_id, &state.metadata).await?;
        let provider_ids_json =
            json!({ candidate.provider.to_ascii_lowercase(): candidate.provider_id }).to_string();
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
                metadata_fingerprint: &nfo_report.fingerprint,
                provenance_json: &state.provenance_json(),
                locked_fields_json: &state.locked_fields_json(),
            })
            .await?;
        if !selected {
            return Err(MetadataSelectionError::CandidateNotPending(
                "CONCURRENTLY_SELECTED".to_owned(),
            ));
        }
        let has_thumbnail =
            image_types.contains(&"THUMB") || self.images.has_local_image(item_id, "THUMB").await?;
        self.database
            .set_thumbnail_fallback_required(item_id, !has_thumbnail)
            .await?;
        Ok(MetadataSelectionReport {
            item_id: item_id.to_owned(),
            candidate_id: candidate_id.to_owned(),
            mode,
            status: "ONLINE_CONFIRMED",
            image_types,
            actor_count,
        })
    }

    async fn write_selected_image(
        &self,
        item_id: &str,
        image_type: &str,
        url: &str,
        source: &str,
        mode: MetadataSelectionMode,
    ) -> Result<Option<crate::application::images::ImageWriteReport>, ImageWriteError> {
        match mode {
            MetadataSelectionMode::FillMissing => {
                self.images
                    .download_item_image_if_missing_from_scraper(item_id, image_type, url, source)
                    .await
            }
            MetadataSelectionMode::RefreshUnlocked => self
                .images
                .download_item_image_from_scraper(item_id, image_type, url, source)
                .await
                .map(Some),
        }
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
    let Some(scraper) = current.scraper_id.as_deref() else {
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

#[derive(Clone, Copy, Debug, Default)]
struct ImageSelectionPolicy {
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
    let premiere_date = candidate_text(&value, &["premiereDate", "releaseDate", "release_date"]);
    let end_date = candidate_text(
        &value,
        &["endDate", "end_date", "lastAirDate", "last_air_date"],
    );
    let status = candidate_text(&value, &["status", "Status"]);
    let original_language = candidate_text(&value, &["originalLanguage", "original_language"]);
    let rating = candidate_rating(&value)?;
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
        && images.values().all(Vec::is_empty)
        && poster_url.is_none()
        && fanart_url.is_none()
        && actors.is_empty()
    {
        return Err(MetadataSelectionError::InvalidCandidate(
            "candidate contains no writable metadata or images".to_owned(),
        ));
    }
    Ok(CandidatePayload {
        metadata,
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

#[cfg(test)]
fn tmdb_candidate_actors(cast: &[TmdbCastMember]) -> Vec<ActorCredit> {
    cast.iter()
        .take(12)
        .filter_map(|member| {
            let name = member.name.as_deref()?.trim();
            if member.id <= 0 || name.is_empty() {
                return None;
            }
            Some(ActorCredit {
                id: member.id.to_string(),
                name: name.to_owned(),
                character: member
                    .character
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned),
                order: member.order,
                profile_url: member
                    .profile_path
                    .as_deref()
                    .filter(|path| path.starts_with('/') && path.len() > 1)
                    .map(|path| format!("https://image.tmdb.org/t/p/w185{path}")),
            })
        })
        .collect()
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
        .take(12)
        .map(|actor| {
            let actor = serde_json::from_value::<ActorCredit>(actor.clone()).map_err(|error| {
                MetadataSelectionError::InvalidCandidate(format!("actor is invalid: {error}"))
            })?;
            if actor.id.trim().is_empty() || actor.name.trim().is_empty() {
                return Err(MetadataSelectionError::InvalidCandidate(
                    "actor ID and name are required".to_owned(),
                ));
            }
            Ok(actor)
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
        .filter(|value| !value.is_empty())
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
        TmdbCastMember, default_image_selection_policy, generic_candidate_images,
        tmdb_candidate_actors,
    };
    use crate::application::scraper::{ScraperImage, ScraperItemType};

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
    fn tmdb_cast_becomes_ordered_candidate_actor_data() {
        let actors = tmdb_candidate_actors(&[
            TmdbCastMember {
                id: 9,
                name: Some(" 演员甲 ".to_owned()),
                character: Some(" 角色甲 ".to_owned()),
                profile_path: Some("/profile.jpg".to_owned()),
                order: Some(0),
            },
            TmdbCastMember {
                id: 10,
                name: Some("演员乙".to_owned()),
                character: None,
                profile_path: None,
                order: Some(1),
            },
        ]);

        assert_eq!(actors[0].name, "演员甲");
        assert_eq!(actors[0].character.as_deref(), Some("角色甲"));
        assert_eq!(
            actors[0].profile_url.as_deref(),
            Some("https://image.tmdb.org/t/p/w185/profile.jpg")
        );
        assert_eq!(actors[1].id, "10");
    }
}
