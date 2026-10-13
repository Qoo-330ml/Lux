use crate::{
    application::{
        admin_cache,
        catalog::CatalogService,
        deletion::{MediaDeleteError, MediaDeleteReport, MediaDeleteService},
        home::HomeService,
        item_merge::ItemMergeService,
    },
    storage::{StorageError, StoredMediaMerge},
};

pub(crate) async fn delete_media_item(
    deletion: &MediaDeleteService,
    home: Option<&HomeService>,
    item_id: &str,
    source_id: Option<&str>,
) -> Result<MediaDeleteReport, MediaDeleteError> {
    let report = deletion.delete(item_id, source_id).await?;
    admin_cache::invalidate_home(home);
    Ok(report)
}

pub(crate) async fn merge_media_items(
    merge: &ItemMergeService,
    home: Option<&HomeService>,
    catalog: Option<&CatalogService>,
    primary_item_id: &str,
    item_ids: &[String],
) -> Result<StoredMediaMerge, StorageError> {
    let result = merge.merge(primary_item_id, item_ids).await?;
    admin_cache::invalidate_media_catalog(home, catalog);
    Ok(result)
}
