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
    let Some((library_strategy, global_strategy)) =
        database.find_item_media_strategy_settings(item_id).await?
    else {
        return Ok(false);
    };
    let stored = library_strategy.as_deref().or(global_strategy.as_deref());
    Ok(stored
        .and_then(|value| serde_json::from_str::<StoredMediaStrategy>(value).ok())
        .is_some_and(|strategy| strategy.images.write_to_metadata))
}

#[cfg(test)]
mod tests {
    use super::item_metadata_writeback_enabled;
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

    #[tokio::test]
    async fn item_metadata_writeback_strategy_uses_one_query()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Writeback", LibraryKind::Movie, false)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind("writeback-item")
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        sqlx::query(
            "UPDATE libraries
             SET media_strategy_json = ?
             WHERE id = ?",
        )
        .bind(r#"{"images":{"writeToMetadata":true}}"#)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

        database.reset_query_count();
        assert!(item_metadata_writeback_enabled(&database, "writeback-item").await?);
        assert_eq!(database.query_count(), 1);
        Ok(())
    }
}
