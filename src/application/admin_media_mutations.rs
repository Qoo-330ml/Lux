use crate::{
    application::{
        admin_cache,
        catalog::CatalogService,
        deletion::{MediaDeleteError, MediaDeleteReport, MediaDeleteService},
        home::HomeService,
        images::{ImageWriteError, ImageWriteService},
        item_merge::ItemMergeService,
    },
    storage::{Database, StorageError, StoredItemImage, StoredMediaMerge},
};

pub(crate) enum AdminItemImageListError {
    Unavailable,
    ItemNotFound,
    Image(ImageWriteError),
}

pub(crate) async fn list_admin_item_images(
    database: Option<&Database>,
    images: Option<&ImageWriteService>,
    item_id: &str,
) -> Result<Vec<StoredItemImage>, AdminItemImageListError> {
    let database = database.ok_or(AdminItemImageListError::Unavailable)?;
    match database.find_media_item_metadata(item_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(AdminItemImageListError::ItemNotFound),
        Err(_) => return Err(AdminItemImageListError::Unavailable),
    }
    let images = images.ok_or(AdminItemImageListError::Unavailable)?;
    images
        .list_item_images(item_id)
        .await
        .map_err(AdminItemImageListError::Image)
}

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
