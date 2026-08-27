use serde::Deserialize;

use crate::storage::{Database, StorageError};

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredMediaImageSettings {
    #[serde(default)]
    write_to_metadata: bool,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredMediaStrategy {
    #[serde(default)]
    images: StoredMediaImageSettings,
}

pub(crate) async fn item_metadata_writeback_enabled(
    database: &Database,
    item_id: &str,
) -> Result<bool, StorageError> {
    let Some(library_id) = database.find_item_library_id(item_id).await? else {
        return Ok(false);
    };
    let Some(library) = database.find_library(&library_id).await? else {
        return Ok(false);
    };
    let global = database.media_strategy_settings().await?;
    let stored = library.media_strategy_json.as_deref().or(global.as_deref());
    Ok(stored
        .and_then(|value| serde_json::from_str::<StoredMediaStrategy>(value).ok())
        .is_some_and(|strategy| strategy.images.write_to_metadata))
}
