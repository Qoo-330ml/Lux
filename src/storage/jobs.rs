use super::*;

#[path = "jobs/local_metadata.rs"]
mod local_metadata;
use local_metadata::MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES;
pub(crate) use local_metadata::StoredScanLocalMetadataBackfillPage;
#[path = "jobs/metadata_reidentify.rs"]
mod metadata_reidentify;
use local_metadata::escape_sql_like_pattern;
#[path = "jobs/scan.rs"]
mod scan;
#[cfg(test)]
use scan::sidecar_target_query;
#[path = "jobs/shutdown.rs"]
mod shutdown;

#[path = "jobs/strm_probe.rs"]
mod strm_probe;

#[cfg(test)]
pub(crate) use super::media_sources::MAX_MEDIA_SOURCE_DELETE_BATCH_SIZE;
use std::{collections::HashMap, time::Instant};

impl Database {
    pub(crate) const LEGACY_SCAN_REQUIRES_NEW_MANIFEST: &'static str =
        "LEGACY_SCAN_REQUIRES_NEW_MANIFEST";
}

impl Database {
    pub(crate) async fn restore_media_item(&self, item_id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET removed_at = NULL, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(item_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }
}

#[cfg(test)]
#[path = "jobs/tests.rs"]
mod tests;
