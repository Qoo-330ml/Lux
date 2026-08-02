use std::fmt;

use serde_json::{Value, json};

use crate::storage::{Database, StorageError, StoredMediaMetadata, StoredMetadataCandidate};

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
