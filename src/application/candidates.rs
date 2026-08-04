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
        metadata::{MetadataCandidate, MetadataSource, MetadataState, NfoMetadata},
        nfo::{NfoWriteError, NfoWriteService},
        people::{ActorCredit, PeopleError},
        tmdb::{TmdbCastMember, TmdbClient, TmdbError},
    },
    storage::{
        Database, NewMetadataCandidate, SelectedMetadataUpdate, StorageError, StoredMediaMetadata,
        StoredMetadataCandidate,
    },
};

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
        tmdb: &TmdbClient,
    ) -> Result<MetadataCandidatePage, MetadataCandidateError> {
        let current = self
            .database
            .find_media_item_metadata(item_id)
            .await?
            .ok_or(MetadataCandidateError::ItemNotFound)?;
        let query = query.trim();
        if query.is_empty() || query.chars().count() > 128 {
            return Err(MetadataCandidateError::InvalidSearch);
        }
        if year.is_some_and(|value| !(1800..=2200).contains(&value)) {
            return Err(MetadataCandidateError::InvalidSearch);
        }

        let response = tmdb
            .search_movies_with_english_fallback(query, year)
            .await
            .map_err(MetadataCandidateError::Tmdb)?;
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_secs()).ok())
            .and_then(|now| now.checked_add(24 * 60 * 60));
        for result in response.results.into_iter().take(20) {
            let title = result
                .title
                .clone()
                .or_else(|| result.original_title.clone())
                .unwrap_or_else(|| query.to_owned());
            let score = if same_title(&current.title, &title)
                || result
                    .original_title
                    .as_deref()
                    .is_some_and(|value| same_title(&current.title, value))
            {
                80.0
            } else {
                0.0
            };
            let production_year = result
                .release_date
                .as_deref()
                .and_then(|value| value.get(..4))
                .and_then(|value| value.parse::<i32>().ok());
            let images = tmdb
                .movie_images(result.id, "zh-CN")
                .await
                .unwrap_or_default();
            let actors = tmdb
                .movie_credits(result.id, "zh-CN")
                .await
                .map(|credits| tmdb_candidate_actors(&credits.cast))
                .unwrap_or_default();
            let candidate_json = json!({
                "title": title,
                "originalTitle": result.original_title,
                "overview": result.overview,
                "releaseDate": result.release_date,
                "productionYear": production_year,
                "originalLanguage": result.original_language,
                "images": tmdb_candidate_images(&images),
                "actors": actors,
            })
            .to_string();
            let id = uuid::Uuid::now_v7().to_string();
            let provider_id = result.id.to_string();
            self.database
                .insert_metadata_candidate(NewMetadataCandidate {
                    id: &id,
                    item_id,
                    provider: "TMDB",
                    provider_id: &provider_id,
                    candidate_json: &candidate_json,
                    score,
                    expires_at,
                })
                .await?;
        }
        self.list_for_item(item_id, None, 0, 50).await
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

fn same_title(left: &str, right: &str) -> bool {
    left.trim().eq_ignore_ascii_case(right.trim())
}

fn tmdb_candidate_images(
    images: &crate::application::tmdb::TmdbImagesResponse,
) -> BTreeMap<String, Vec<String>> {
    let posters = tmdb_image_urls(&images.posters);
    let backdrops = tmdb_image_urls(&images.backdrops);
    let logos = tmdb_image_urls(&images.logos);
    [
        ("POSTER", posters.clone()),
        ("LOGO", logos),
        ("THUMB", backdrops.clone()),
        ("BANNER", backdrops.clone()),
        ("DISC", posters),
        ("ART", backdrops.clone()),
        ("WALLPAPER", backdrops),
    ]
    .into_iter()
    .map(|(image_type, urls)| (image_type.to_owned(), urls))
    .collect()
}

fn tmdb_image_urls(images: &[crate::application::tmdb::TmdbImageReference]) -> Vec<String> {
    images
        .iter()
        .filter_map(|image| {
            let path = image.file_path.as_deref()?.trim();
            (path.starts_with('/') && path.len() > 1)
                .then(|| format!("https://image.tmdb.org/t/p/w780{path}"))
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
        let mut state = MetadataState::from_persisted(
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
        );
        let metadata_candidate = MetadataCandidate {
            source: MetadataSource::TmdbLocalized,
            metadata: payload.metadata,
        };
        match mode {
            MetadataSelectionMode::FillMissing => state.apply_fill_missing(&metadata_candidate),
            MetadataSelectionMode::RefreshUnlocked => {
                state.apply_refresh_unlocked(&metadata_candidate)
            }
        }
        let nfo_report = self.nfo.write_item_nfo(item_id, &state.metadata).await?;
        let mut image_types = Vec::new();
        if payload.typed_images_present {
            for image_type in image_policy.enabled_types() {
                let Some(url) = payload.images.get(image_type).and_then(|urls| urls.first()) else {
                    continue;
                };
                if self
                    .write_selected_image(item_id, image_type, url, mode)
                    .await?
                    .is_some()
                {
                    image_types.push(image_type);
                }
            }
        } else {
            if let Some(url) = payload.poster_url.as_deref() {
                if self
                    .write_selected_image(item_id, "POSTER", url, mode)
                    .await?
                    .is_some()
                {
                    image_types.push("POSTER");
                }
            }
            if let Some(url) = payload.fanart_url.as_deref() {
                if self
                    .write_selected_image(item_id, "FANART", url, mode)
                    .await?
                    .is_some()
                {
                    image_types.push("FANART");
                }
            }
        }
        let actor_count = self
            .people
            .persist_item_actors(item_id, &payload.actors)
            .await?;
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
        mode: MetadataSelectionMode,
    ) -> Result<Option<crate::application::images::ImageWriteReport>, ImageWriteError> {
        match mode {
            MetadataSelectionMode::FillMissing => {
                self.images
                    .download_item_image_if_missing(item_id, image_type, url)
                    .await
            }
            MetadataSelectionMode::RefreshUnlocked => self
                .images
                .download_item_image(item_id, image_type, url)
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
        images,
        typed_images_present,
        poster_url,
        fanart_url,
        actors,
    })
}

fn tmdb_candidate_actors(cast: &[TmdbCastMember]) -> Vec<ActorCredit> {
    cast.iter()
        .take(12)
        .filter_map(|member| {
            let name = member.name.as_deref()?.trim();
            if member.id <= 0 || name.is_empty() {
                return None;
            }
            Some(ActorCredit {
                id: member.id,
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
            if actor.id <= 0 || actor.name.trim().is_empty() {
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
    use super::{TmdbCastMember, tmdb_candidate_actors};

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
        assert_eq!(actors[1].id, 10);
    }
}
