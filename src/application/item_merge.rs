use crate::storage::{Database, StorageError, StoredMediaMerge};

#[derive(Clone)]
pub struct ItemMergeService {
    database: Database,
}

impl ItemMergeService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub(crate) async fn merge(
        &self,
        primary_item_id: &str,
        item_ids: &[String],
    ) -> Result<StoredMediaMerge, StorageError> {
        self.database
            .merge_media_items(primary_item_id, item_ids)
            .await
    }
}
