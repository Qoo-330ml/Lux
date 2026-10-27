//! Rebuild orchestration for the `catalog_items` read model.
//!
//! The read model is derived data: a rebuild can run at any time, resumes from the last processed
//! item id, and never blocks a request path. Consumers for incremental changes are wired in
//! LUX-865; until then this module only provides the full rebuild and its validation summary.

use crate::storage::{Database, StorageError};

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct CatalogRebuildReport {
    pub(crate) items: i64,
    pub(crate) content_sha256: String,
}

/// Rebuilds every row and the library/global counters, returning the validation summary.
#[allow(dead_code)] // used by tests and the future manual rebuild endpoint
pub(crate) async fn rebuild_catalog_read_model(
    database: &Database,
    batch: i64,
) -> Result<CatalogRebuildReport, StorageError> {
    rebuild_catalog_read_model_from(database, batch, None).await
}

/// Resumes a rebuild after `after`; startup uses the highest materialized id so an interrupted
/// rebuild only redoes the missing tail.
async fn rebuild_catalog_read_model_from(
    database: &Database,
    batch: i64,
    after: Option<String>,
) -> Result<CatalogRebuildReport, StorageError> {
    let mut after = after;
    loop {
        let (processed, last) = database
            .rebuild_catalog_items_batch(after.as_deref(), batch)
            .await?;
        if processed == 0 {
            break;
        }
        after = last;
    }
    database.rebuild_catalog_item_stats().await?;
    let summary = database.catalog_items_checksum().await?;
    // Marks the rebuild as complete; an interrupted rebuild leaves no watermark and is retried.
    database
        .advance_catalog_consumer("catalog_items_rebuild", summary.item_count)
        .await?;
    Ok(CatalogRebuildReport {
        items: summary.item_count,
        content_sha256: summary.content_sha256,
    })
}

/// Fills an empty read model once, in the background. Failures are logged; the next start retries.
pub(crate) fn spawn_startup_catalog_read_model(database: Database) {
    tokio::spawn(async move {
        let rebuild_done = match database.catalog_consumer("catalog_items_rebuild").await {
            Ok(state) => state.is_some(),
            Err(error) => {
                tracing::warn!(%error, "catalog read model watermark read failed");
                false
            }
        };
        if rebuild_done {
            return;
        }
        {
            let started = std::time::Instant::now();
            let resume_after = database.catalog_items_max_item_id().await.unwrap_or(None);
            match rebuild_catalog_read_model_from(&database, 1_000, resume_after).await {
                Ok(report) => tracing::info!(
                    items = report.items,
                    checksum = %report.content_sha256,
                    elapsed_ms = started.elapsed().as_millis(),
                    "catalog read model rebuilt"
                ),
                Err(error) => tracing::warn!(%error, "catalog read model rebuild failed"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::libraries::LibraryService;
    use crate::library::LibraryKind;
    use crate::storage::test_database;

    #[tokio::test]
    async fn rebuild_reports_row_count_and_stable_checksum()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Rebuild", LibraryKind::Movie, false)
            .await?;
        for index in 0..3 {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status,
                     has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
            )
            .bind(format!("rebuild-{index}"))
            .bind(library.id.to_string())
            .bind(format!("Rebuild {index}"))
            .bind(format!("rebuild {index}"))
            .execute(database.pool())
            .await?;
        }

        let first = rebuild_catalog_read_model(&database, 2).await?;
        assert_eq!(first.items, 3);
        assert_eq!(first.content_sha256.len(), 64);
        let second = rebuild_catalog_read_model(&database, 2).await?;
        assert_eq!(second, first);
        Ok(())
    }
}
