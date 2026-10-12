use super::*;
#[path = "local_metadata/backfill.rs"]
mod backfill;

pub(crate) use backfill::StoredScanLocalMetadataBackfillPage;

fn parse_scan_local_metadata_non_retryable_item_ids(
    value: &str,
    max_count: usize,
) -> Result<Vec<String>, StorageError> {
    let item_ids = serde_json::from_str::<Vec<String>>(value).map_err(|error| {
        StorageError::Conflict(format!("invalid scan metadata exclusions: {error}"))
    })?;
    if item_ids.len() > max_count
        || item_ids.iter().any(|item_id| item_id.trim().is_empty())
        || item_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != item_ids.len()
    {
        return Err(StorageError::Conflict(
            "scan metadata exclusions are invalid".into(),
        ));
    }
    Ok(item_ids)
}

pub(super) fn serialize_scan_local_metadata_non_retryable_item_ids(
    item_ids: &[String],
    max_count: usize,
) -> Result<String, StorageError> {
    if item_ids.len() > max_count
        || item_ids.iter().any(|item_id| item_id.trim().is_empty())
        || item_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != item_ids.len()
    {
        return Err(StorageError::Conflict(
            "scan metadata exclusions are invalid".into(),
        ));
    }
    serde_json::to_string(item_ids).map_err(|error| StorageError::Serialization(error.to_string()))
}
