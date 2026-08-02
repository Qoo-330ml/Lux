use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    application::{
        images::{ImageWriteError, ImageWriteService},
        metadata::{MetadataCandidate, MetadataSource, MetadataState, NfoMetadata},
        nfo::{NfoWriteError, NfoWriteService},
    },
    storage::{
        Database, SelectedMetadataUpdate, StorageError, StoredMediaMetadata,
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
}

#[derive(Clone)]
pub struct MetadataSelectionService {
    database: Database,
    nfo: NfoWriteService,
    images: ImageWriteService,
}

impl MetadataSelectionService {
    pub fn new(database: Database, images: ImageWriteService) -> Self {
        Self {
            nfo: NfoWriteService::new(database.clone()),
            database,
            images,
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
        if let Some(url) = payload.poster_url.as_deref() {
            let report = match mode {
                MetadataSelectionMode::FillMissing => {
                    self.images
                        .download_item_image_if_missing(item_id, "POSTER", url)
                        .await?
                }
                MetadataSelectionMode::RefreshUnlocked => Some(
                    self.images
                        .download_item_image(item_id, "POSTER", url)
                        .await?,
                ),
            };
            if report.is_some() {
                image_types.push("POSTER");
            }
        }
        if let Some(url) = payload.fanart_url.as_deref() {
            let report = match mode {
                MetadataSelectionMode::FillMissing => {
                    self.images
                        .download_item_image_if_missing(item_id, "FANART", url)
                        .await?
                }
                MetadataSelectionMode::RefreshUnlocked => Some(
                    self.images
                        .download_item_image(item_id, "FANART", url)
                        .await?,
                ),
            };
            if report.is_some() {
                image_types.push("FANART");
            }
        }
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
        })
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
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MetadataSelectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Nfo(error) => Some(error),
            Self::Image(error) => Some(error),
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

struct CandidatePayload {
    metadata: NfoMetadata,
    poster_url: Option<String>,
    fanart_url: Option<String>,
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
    if metadata.title.is_none()
        && metadata.original_title.is_none()
        && metadata.overview.is_none()
        && metadata.production_year.is_none()
        && poster_url.is_none()
        && fanart_url.is_none()
    {
        return Err(MetadataSelectionError::InvalidCandidate(
            "candidate contains no writable metadata or images".to_owned(),
        ));
    }
    Ok(CandidatePayload {
        metadata,
        poster_url,
        fanart_url,
    })
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
