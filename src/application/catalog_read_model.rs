//! Rebuild orchestration for the `catalog_items` read model.
//!
//! The read model is derived data: a rebuild can run at any time, resumes from the last processed
//! item id, and never blocks a request path. Consumers for incremental changes are wired in
//! LUX-865; until then this module only provides the full rebuild and its validation summary.

use std::{sync::Arc, time::Duration};

use tokio::{sync::watch, task::JoinHandle};

use crate::storage::{
    CATALOG_ITEMS_INCREMENTAL_CONSUMER, CATALOG_OUTBOX, Database, SEARCH_OUTBOX,
    SEARCH_REINDEX_CONSUMER, StorageError,
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
///
/// Startup uses `rebuild_catalog_read_model_from` with a resume point; only the crate's storage
/// regression tests drive the unbounded-from-the-start wrapper.
#[cfg(test)]
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
        .advance_catalog_consumer(&catalog_items_consumer(), summary.item_count)
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

/// How many queued events one consumer pass applies.
const SEARCH_REINDEX_BATCH: i64 = 500;
/// Applied queue rows are reclaimed in bounded batches on the same pass.
const SEARCH_REINDEX_TRIM_BATCH: i64 = 2_000;

const CATALOG_ITEMS_INCREMENTAL_BATCH: i64 = 500;
const CATALOG_ITEMS_TRIM_BATCH: i64 = 2_000;
/// Rows of the retired shared search stream still sitting in `catalog_outbox` are deleted in bounded
/// batches, a few batches per pass, until none are left.
const LEGACY_SEARCH_PURGE_BATCH: i64 = 2_000;
const LEGACY_SEARCH_PURGE_BATCHES_PER_PASS: usize = 10;
/// Counters are recomputed at most this often: the grouping scans the whole derived table, while a
/// stale count is only cosmetic for a few seconds.
const CATALOG_ITEMS_STATS_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// How long an idle outbox consumer sleeps before it checks the queue on its own.
///
/// Every in-process write signals the consumer on commit (see `storage::OutboxSignals`), so this
/// timer is not the freshness path: it only covers writes this process cannot observe, such as a
/// second process sharing the database or a database restored from a backup file. Ten seconds keeps
/// that gap bounded without waking the process while it is idle.
const OUTBOX_IDLE_FALLBACK: Duration = Duration::from_secs(10);

/// The two derived-data consumer loops (`catalog_items` and the search index).
///
/// Both stop on [`Self::shutdown`], which the application calls while draining its background
/// workers, so a stop can no longer interrupt a write halfway.
#[derive(Clone)]
pub(crate) struct CatalogOutboxWorkers {
    shutdown_sender: watch::Sender<bool>,
    tasks: Arc<tokio::sync::Mutex<Vec<JoinHandle<()>>>>,
}

impl CatalogOutboxWorkers {
    pub(crate) fn start(database: Database) -> Self {
        Self::start_with_fallback(database, OUTBOX_IDLE_FALLBACK)
    }

    fn start_with_fallback(database: Database, fallback: Duration) -> Self {
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let tasks = vec![
            tokio::spawn(run_catalog_items_incremental_worker(
                database.clone(),
                shutdown_receiver.clone(),
                fallback,
            )),
            tokio::spawn(run_search_reindex_worker(
                database,
                shutdown_receiver,
                fallback,
            )),
        ];
        Self {
            shutdown_sender,
            tasks: Arc::new(tokio::sync::Mutex::new(tasks)),
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.shutdown_sender.send_replace(true);
        let tasks = {
            let mut tasks = self.tasks.lock().await;
            std::mem::take(&mut *tasks)
        };
        for task in tasks {
            if let Err(error) = task.await {
                tracing::warn!(%error, "catalog outbox worker stopped unexpectedly");
            }
        }
    }
}

/// Applies queued `kind = 'catalog'` events so inserts, metadata updates, deletions and merges reach
/// `catalog_items` without waiting for the next full rebuild.
///
/// The loop drains until the queue is empty and only then waits, so a backlog is applied
/// continuously instead of one batch per timer tick. Waiting is event driven: a committing writer
/// signals it, and `fallback` only covers writes this process cannot observe.
async fn run_catalog_items_incremental_worker(
    database: Database,
    mut shutdown: watch::Receiver<bool>,
    fallback: Duration,
) {
    let signals = database.outbox_signals();
    let mut stats_dirty = false;
    let mut legacy_search_purged = false;
    let mut last_stats_refresh = std::time::Instant::now();
    loop {
        if *shutdown.borrow() {
            return;
        }
        let mut worked = false;
        if !legacy_search_purged {
            match purge_legacy_search_rows(&database).await {
                Ok(true) => legacy_search_purged = true,
                // More rows left: keep going on this pass instead of sleeping a full fallback.
                Ok(false) => worked = true,
                // Unreadable table: fall through to the ordinary wait, so a persistent failure
                // backs off instead of spinning.
                Err(()) => {}
            }
        }
        // Only trust derived rows once the full rebuild watermark exists: the consumer must never
        // mix incremental rows into a half-built table.
        let rebuilt = matches!(
            database.catalog_consumer(&catalog_items_consumer()).await,
            Ok(Some(_))
        );
        if rebuilt {
            loop {
                if *shutdown.borrow() {
                    return;
                }
                match catalog_items_step(&database).await {
                    Ok(0) => break,
                    Ok(items) => {
                        stats_dirty = true;
                        worked = true;
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
        }
        // The counters are rebuilt at most once per interval, but a pending refresh must not wait
        // for the much longer idle fallback.
        let mut wait = fallback;
        if stats_dirty {
            let elapsed = last_stats_refresh.elapsed();
            if elapsed >= CATALOG_ITEMS_STATS_REFRESH_INTERVAL {
                match database.rebuild_catalog_item_stats().await {
                    Ok(()) => {
                        stats_dirty = false;
                        last_stats_refresh = std::time::Instant::now();
                    }
                    Err(error) => tracing::warn!(%error, "catalog item stats refresh failed"),
                }
            } else if !worked {
                wait = wait.min(CATALOG_ITEMS_STATS_REFRESH_INTERVAL - elapsed);
            }
        }
        if worked {
            continue;
        }
        tokio::select! {
            () = signals.notified_catalog_items() => {}
            () = tokio::time::sleep(wait) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// Drains queued search-reindex events so a scan leaves a short queue instead of writing the FTS
/// index inside every item transaction. See [`run_catalog_items_incremental_worker`] for the wait
/// strategy.
async fn run_search_reindex_worker(
    database: Database,
    mut shutdown: watch::Receiver<bool>,
    fallback: Duration,
) {
    let signals = database.outbox_signals();
    loop {
        if *shutdown.borrow() {
            return;
        }
        let mut worked = false;
        loop {
            if *shutdown.borrow() {
                return;
            }
            match search_reindex_step(&database).await {
                Ok(0) => break,
                Ok(items) => {
                    worked = true;
                    tracing::debug!(items, "search reindex applied");
                }
                Err(error) => {
                    tracing::warn!(%error, "search reindex failed");
                    let _ = database
                        .record_catalog_consumer_error(SEARCH_REINDEX_CONSUMER, &error.to_string())
                        .await;
                    break;
                }
            }
        }
        if worked {
            continue;
        }
        tokio::select! {
            () = signals.notified_search_reindex() => {}
            () = tokio::time::sleep(fallback) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

/// One bounded unit of catalog consumer work: apply a batch, then reclaim the rows it applied.
///
/// The trim runs on every step instead of only once the queue is empty. A consumer that is
/// permanently behind never sees an empty queue, so tying the trim to "drained" meant a backlog
/// could only ever grow.
pub(crate) async fn catalog_items_step(database: &Database) -> Result<usize, StorageError> {
    let items = database
        .drain_catalog_items_incremental(CATALOG_ITEMS_INCREMENTAL_BATCH)
        .await?;
    trim_applied(
        database,
        CATALOG_ITEMS_INCREMENTAL_CONSUMER,
        CATALOG_OUTBOX,
        CATALOG_ITEMS_TRIM_BATCH,
    )
    .await;
    Ok(items)
}

/// One bounded unit of search consumer work; see [`catalog_items_step`].
pub(crate) async fn search_reindex_step(database: &Database) -> Result<usize, StorageError> {
    let items = database.drain_search_reindex(SEARCH_REINDEX_BATCH).await?;
    trim_applied(
        database,
        SEARCH_REINDEX_CONSUMER,
        SEARCH_OUTBOX,
        SEARCH_REINDEX_TRIM_BATCH,
    )
    .await;
    Ok(items)
}

/// Reclaims applied rows. A failed trim only delays reclamation, so it never fails the step.
async fn trim_applied(
    database: &Database,
    consumer: &str,
    stream: crate::storage::OutboxStream,
    batch: i64,
) {
    match database.trim_outbox(consumer, stream, batch).await {
        Ok(0) => {}
        Ok(trimmed) => tracing::debug!(trimmed, table = stream.table, "outbox trimmed"),
        Err(error) => tracing::warn!(%error, table = stream.table, "outbox trim failed"),
    }
}

/// Deletes a few batches of retired `search` rows from `catalog_outbox`.
///
/// Returns `Ok(true)` once none are left, `Ok(false)` when another pass is needed and `Err(())` when
/// the table cannot be read, so the caller can keep draining a backlog without spinning on a
/// failure.
async fn purge_legacy_search_rows(database: &Database) -> Result<bool, ()> {
    for _ in 0..LEGACY_SEARCH_PURGE_BATCHES_PER_PASS {
        match database
            .purge_legacy_search_outbox(LEGACY_SEARCH_PURGE_BATCH)
            .await
        {
            Ok(0) => return Ok(true),
            Ok(purged) => tracing::debug!(purged, "legacy search outbox rows purged"),
            Err(error) => {
                tracing::warn!(%error, "legacy search outbox purge failed");
                return Err(());
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::libraries::LibraryService;
    use crate::library::LibraryKind;
    use crate::storage::test_database;

    /// A fallback far longer than any test timeout: a pass can then only come from a write signal.
    const LONG_FALLBACK: Duration = Duration::from_secs(60);

    async fn add_movie(
        database: &Database,
        item_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let library = LibraryService::new(database.clone())
            .create_library("Signals", LibraryKind::Movie, false)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 has_available_source
             ) VALUES (?, ?, 'MOVIE', 'Signals', 'signals', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(item_id)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        Ok(())
    }

    async fn wait_for_rows(
        database: &Database,
        table: &str,
    ) -> Result<i64, Box<dyn std::error::Error>> {
        let query = format!("SELECT COUNT(*) FROM {table}");
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
            .fetch_one(database.pool())
            .await?;
        Ok(count)
    }

    #[tokio::test]
    async fn idle_catalog_worker_does_not_query_the_database()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, database) = test_database().await?;
        database
            .advance_catalog_consumer(&catalog_items_consumer(), 0)
            .await?;
        let (shutdown_sender, shutdown) = watch::channel(false);
        let worker = tokio::spawn(run_catalog_items_incremental_worker(
            database.clone(),
            shutdown,
            LONG_FALLBACK,
        ));
        // The first pass (legacy purge and the "is the read model built" check) runs immediately.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let before = database.query_count();
        // Longer than the one-second tick this worker used to run on.
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert_eq!(
            database.query_count(),
            before,
            "an idle worker must wait for a write signal, not poll"
        );
        shutdown_sender.send_replace(true);
        tokio::time::timeout(Duration::from_secs(2), worker).await??;
        Ok(())
    }

    #[tokio::test]
    async fn idle_search_worker_does_not_query_the_database()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, database) = test_database().await?;
        let (shutdown_sender, shutdown) = watch::channel(false);
        let worker = tokio::spawn(run_search_reindex_worker(
            database.clone(),
            shutdown,
            LONG_FALLBACK,
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let before = database.query_count();
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        assert_eq!(
            database.query_count(),
            before,
            "an idle worker must wait for a write signal, not poll"
        );
        shutdown_sender.send_replace(true);
        tokio::time::timeout(Duration::from_secs(2), worker).await??;
        Ok(())
    }

    async fn applied_seq(
        database: &Database,
        consumer: &str,
    ) -> Result<i64, Box<dyn std::error::Error>> {
        Ok(database
            .catalog_consumer(consumer)
            .await?
            .map(|state| state.applied_seq)
            .unwrap_or(0))
    }

    /// Waits until a worker has drained everything, then snapshots the watermark so a later write
    /// can only advance it through a signal (the fallback is 60 s).
    async fn drained_watermark(
        database: &Database,
        consumer: &str,
    ) -> Result<i64, Box<dyn std::error::Error>> {
        tokio::time::sleep(Duration::from_millis(400)).await;
        applied_seq(database, consumer).await
    }

    async fn waits_for_watermark(
        database: &Database,
        consumer: &str,
        after: i64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if applied_seq(database, consumer).await.unwrap_or(0) > after {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a write signal must apply the queue long before the 60 s fallback");
        Ok(())
    }

    #[tokio::test]
    async fn a_committed_write_reaches_the_read_model_without_the_fallback()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, database) = test_database().await?;
        add_movie(&database, "signalled-movie").await?;
        database
            .advance_catalog_consumer(&catalog_items_consumer(), 0)
            .await?;
        let (shutdown_sender, shutdown) = watch::channel(false);
        let worker = tokio::spawn(run_catalog_items_incremental_worker(
            database.clone(),
            shutdown,
            LONG_FALLBACK,
        ));
        let drained = drained_watermark(&database, CATALOG_ITEMS_INCREMENTAL_CONSUMER).await?;

        // A storage write signals the consumers when it changed a row.
        database
            .adopt_media_item_identity("signalled-movie", "signalled-movie")
            .await?;

        waits_for_watermark(&database, CATALOG_ITEMS_INCREMENTAL_CONSUMER, drained).await?;
        shutdown_sender.send_replace(true);
        tokio::time::timeout(Duration::from_secs(2), worker).await??;
        Ok(())
    }

    #[tokio::test]
    async fn a_committed_write_reaches_the_search_index_without_the_fallback()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, database) = test_database().await?;
        add_movie(&database, "search-movie").await?;
        let (shutdown_sender, shutdown) = watch::channel(false);
        let worker = tokio::spawn(run_search_reindex_worker(
            database.clone(),
            shutdown,
            LONG_FALLBACK,
        ));
        let drained = drained_watermark(&database, SEARCH_REINDEX_CONSUMER).await?;

        // A title change is what queues a search row (the search trigger only covers the title
        // columns), and the write signals the consumer.
        database
            .update_unconfirmed_hierarchy_item(
                "search-movie",
                "Searchable Movie",
                "searchable movie",
                None,
                None,
                None,
            )
            .await?;

        waits_for_watermark(&database, SEARCH_REINDEX_CONSUMER, drained).await?;
        shutdown_sender.send_replace(true);
        tokio::time::timeout(Duration::from_secs(2), worker).await??;
        Ok(())
    }

    #[tokio::test]
    async fn a_backlog_is_drained_continuously_without_waiting_for_a_timer()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, database) = test_database().await?;
        add_movie(&database, "backlog-movie").await?;
        database
            .advance_catalog_consumer(&catalog_items_consumer(), 0)
            .await?;
        // Three full batches: one pass can only apply 500 at a time.
        for table in ["catalog_outbox", "search_outbox"] {
            let kind = if table == "catalog_outbox" {
                "catalog"
            } else {
                "search"
            };
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1200)
                 INSERT INTO {table} (kind, item_id) SELECT '{kind}', 'backlog-movie' FROM n"
            )))
            .execute(database.pool())
            .await?;
        }
        let (shutdown_sender, shutdown) = watch::channel(false);
        let catalog = tokio::spawn(run_catalog_items_incremental_worker(
            database.clone(),
            shutdown.clone(),
            LONG_FALLBACK,
        ));
        let search = tokio::spawn(run_search_reindex_worker(
            database.clone(),
            shutdown,
            LONG_FALLBACK,
        ));
        database.signal_outbox_consumers();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let catalog_left = wait_for_rows(&database, "catalog_outbox")
                    .await
                    .unwrap_or(1);
                let search_left = wait_for_rows(&database, "search_outbox").await.unwrap_or(1);
                if catalog_left == 0 && search_left == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a backlog must be drained by consecutive passes, not one batch per timer tick");
        shutdown_sender.send_replace(true);
        for worker in [catalog, search] {
            tokio::time::timeout(Duration::from_secs(2), worker).await??;
        }
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_stops_the_workers_and_joins_them() -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, database) = test_database().await?;
        database
            .advance_catalog_consumer(&catalog_items_consumer(), 0)
            .await?;
        let workers = CatalogOutboxWorkers::start_with_fallback(database, LONG_FALLBACK);
        tokio::time::timeout(Duration::from_secs(2), workers.shutdown())
            .await
            .expect("shutdown must not wait for the fallback");
        Ok(())
    }

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

    #[tokio::test]
    async fn rebuild_marks_the_version_scoped_watermark_that_readers_check()
    -> Result<(), Box<dyn std::error::Error>> {
        // Readers and the incremental worker look for `catalog_items_consumer()`; a rebuild that
        // records any other name leaves the read model permanently "not ready".
        let (_temp, database) = test_database().await?;
        assert!(
            database
                .catalog_consumer(&catalog_items_consumer())
                .await?
                .is_none()
        );
        rebuild_catalog_read_model(&database, 10).await?;
        assert!(
            database
                .catalog_consumer(&catalog_items_consumer())
                .await?
                .is_some(),
            "rebuild must record the watermark under the version-scoped name"
        );
        Ok(())
    }

    #[tokio::test]
    async fn consumer_steps_reclaim_applied_rows_while_a_backlog_remains()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, database) = test_database().await?;
        for (table, kind) in [("catalog_outbox", "catalog"), ("search_outbox", "search")] {
            // Three batches' worth of events for items that need not exist.
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1200)
                 INSERT INTO {table} (kind, item_id) SELECT '{kind}', 'backlog-' || i FROM n"
            )))
            .execute(database.pool())
            .await?;
        }

        // One step applies one batch and reclaims it, even though the queue is nowhere near empty.
        assert_eq!(catalog_items_step(&database).await?, 500);
        assert_eq!(search_reindex_step(&database).await?, 500);
        for table in ["catalog_outbox", "search_outbox"] {
            let remaining: i64 =
                sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
                    .fetch_one(database.pool())
                    .await?;
            assert_eq!(remaining, 700, "{table} keeps only the unapplied events");
        }
        Ok(())
    }
}
