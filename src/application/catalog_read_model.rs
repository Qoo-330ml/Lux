//! Rebuild orchestration for the `catalog_items` read model.
//!
//! The read model is derived data: a rebuild can run at any time, resumes from the last processed
//! item id, and never blocks a request path. Consumers for incremental changes are wired in
//! LUX-865; until then this module only provides the full rebuild and its validation summary.

use std::time::Duration;

use crate::storage::{
    CATALOG_ITEMS_INCREMENTAL_CONSUMER, CATALOG_OUTBOX_KIND, Database, SEARCH_REINDEX_CONSUMER,
    SEARCH_REINDEX_KIND, StorageError,
};

/// Bumped whenever the derived row shape or its inputs change, so a read model written by an older
/// build is rebuilt instead of trusted.
const READ_MODEL_SCHEMA_VERSION: i64 = 2;

/// Watermark name for the `catalog_items` rebuild; its presence means the read model is complete.
/// The name carries [`READ_MODEL_SCHEMA_VERSION`], so watermarks from older builds are ignored.
pub(crate) fn catalog_items_consumer() -> String {
    format!("catalog_items_rebuild@{READ_MODEL_SCHEMA_VERSION}")
}

/// Watermark name for the derived continue-watching pointers, versioned like the catalog model.
pub(crate) fn user_series_next_consumer() -> String {
    format!("user_series_next_rebuild@{READ_MODEL_SCHEMA_VERSION}")
}

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
        let rebuild_done = match database.catalog_consumer(&catalog_items_consumer()).await {
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

/// Fills the derived continue-watching pointers once, in the background. Failures are logged and
/// retried on the next start; the read switch falls back to the authoritative query until then.
pub(crate) fn spawn_startup_user_series_next(database: Database) {
    tokio::spawn(async move {
        let rebuilt = match database
            .catalog_consumer(&user_series_next_consumer())
            .await
        {
            Ok(state) => state.is_some(),
            Err(error) => {
                tracing::warn!(%error, "user_series_next watermark read failed");
                false
            }
        };
        if rebuilt {
            return;
        }
        let started = std::time::Instant::now();
        match database.rebuild_user_series_next().await {
            Ok(pointers) => {
                tracing::info!(
                    pointers,
                    elapsed_ms = started.elapsed().as_millis(),
                    "user_series_next rebuilt"
                );
                if let Err(error) = database
                    .advance_catalog_consumer(&user_series_next_consumer(), 1)
                    .await
                {
                    tracing::warn!(%error, "user_series_next watermark write failed");
                }
            }
            Err(error) => tracing::warn!(%error, "user_series_next rebuild failed"),
        }
    });
}

/// How many queued events one consumer pass applies, and how often it runs.
const SEARCH_REINDEX_BATCH: i64 = 500;
const SEARCH_REINDEX_INTERVAL: Duration = Duration::from_secs(1);
/// Applied queue rows are reclaimed in bounded batches on the same tick.
const SEARCH_REINDEX_TRIM_BATCH: i64 = 2_000;

/// How often the incremental `catalog_items` consumer runs; an authoritative write therefore
/// reaches the read-model paths within about one tick.
const CATALOG_ITEMS_INCREMENTAL_INTERVAL: Duration = Duration::from_secs(1);
const CATALOG_ITEMS_INCREMENTAL_BATCH: i64 = 500;
const CATALOG_ITEMS_TRIM_BATCH: i64 = 2_000;
/// Counters are recomputed at most this often: the grouping scans the whole derived table, while a
/// stale count is only cosmetic for a few seconds.
const CATALOG_ITEMS_STATS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Applies queued `kind = 'catalog'` events so inserts, metadata updates, deletions and merges reach
/// `catalog_items` within a tick instead of waiting for the next full rebuild.
pub(crate) fn spawn_catalog_items_incremental_worker(database: Database) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(CATALOG_ITEMS_INCREMENTAL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut stats_dirty = false;
        let mut last_stats_refresh = std::time::Instant::now();
        loop {
            ticker.tick().await;
            // Only trust derived rows once the full rebuild watermark exists: the consumer must
            // never mix incremental rows into a half-built table.
            let rebuilt = matches!(
                database.catalog_consumer(&catalog_items_consumer()).await,
                Ok(Some(_))
            );
            if !rebuilt {
                continue;
            }
            loop {
                match database
                    .drain_catalog_items_incremental(CATALOG_ITEMS_INCREMENTAL_BATCH)
                    .await
                {
                    Ok(0) => match database
                        .trim_catalog_outbox(
                            CATALOG_ITEMS_INCREMENTAL_CONSUMER,
                            CATALOG_OUTBOX_KIND,
                            CATALOG_ITEMS_TRIM_BATCH,
                        )
                        .await
                    {
                        Ok(0) => break,
                        Ok(trimmed) => tracing::debug!(trimmed, "catalog outbox trimmed"),
                        Err(error) => {
                            tracing::warn!(%error, "catalog outbox trim failed");
                            break;
                        }
                    },
                    Ok(items) => {
                        stats_dirty = true;
                        tracing::debug!(items, "catalog read model applied");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "catalog read model incremental apply failed");
                        let _ = database
                            .record_catalog_consumer_error(
                                CATALOG_ITEMS_INCREMENTAL_CONSUMER,
                                &error.to_string(),
                            )
                            .await;
                        break;
                    }
                }
            }
            if stats_dirty && last_stats_refresh.elapsed() >= CATALOG_ITEMS_STATS_REFRESH_INTERVAL {
                match database.rebuild_catalog_item_stats().await {
                    Ok(()) => {
                        stats_dirty = false;
                        last_stats_refresh = std::time::Instant::now();
                    }
                    Err(error) => tracing::warn!(%error, "catalog item stats refresh failed"),
                }
            }
        }
    });
}

/// Drains queued search-reindex events in the background so a scan leaves a short queue instead of
/// writing the FTS index inside every item transaction.
pub(crate) fn spawn_search_reindex_worker(database: Database) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(SEARCH_REINDEX_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            loop {
                match database.drain_search_reindex(SEARCH_REINDEX_BATCH).await {
                    Ok(0) => {
                        // Everything queued has been applied; reclaim the rows we no longer need.
                        match database
                            .trim_catalog_outbox(
                                SEARCH_REINDEX_CONSUMER,
                                SEARCH_REINDEX_KIND,
                                SEARCH_REINDEX_TRIM_BATCH,
                            )
                            .await
                        {
                            Ok(0) => break,
                            Ok(trimmed) => tracing::debug!(trimmed, "catalog outbox trimmed"),
                            Err(error) => {
                                tracing::warn!(%error, "catalog outbox trim failed");
                                break;
                            }
                        }
                    }
                    Ok(items) => tracing::debug!(items, "search reindex applied"),
                    Err(error) => {
                        tracing::warn!(%error, "search reindex failed");
                        let _ = database
                            .record_catalog_consumer_error(
                                SEARCH_REINDEX_CONSUMER,
                                &error.to_string(),
                            )
                            .await;
                        break;
                    }
                }
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
    async fn read_model_watermarks_are_version_scoped() -> Result<(), Box<dyn std::error::Error>> {
        // A watermark written by an older build must not make the `on` paths trust derived rows.
        let (_temp, database) = test_database().await?;
        database
            .advance_catalog_consumer("catalog_items_rebuild", 10)
            .await?;
        database
            .advance_catalog_consumer("user_series_next_rebuild", 1)
            .await?;
        assert!(
            database
                .catalog_consumer(&catalog_items_consumer())
                .await?
                .is_none()
        );
        assert!(
            database
                .catalog_consumer(&user_series_next_consumer())
                .await?
                .is_none()
        );

        database
            .advance_catalog_consumer(&catalog_items_consumer(), 10)
            .await?;
        database
            .advance_catalog_consumer(&user_series_next_consumer(), 1)
            .await?;
        assert!(
            database
                .catalog_consumer(&catalog_items_consumer())
                .await?
                .is_some()
        );
        assert!(
            database
                .catalog_consumer(&user_series_next_consumer())
                .await?
                .is_some()
        );
        Ok(())
    }

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
