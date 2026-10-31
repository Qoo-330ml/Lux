//! In-process wake-up signals for the derived-data outbox consumers.
//!
//! Three background consumers drain queues that authoritative writes fill: the `catalog_items`
//! read model, the search reindex stream and the global collection sync outbox. The queue rows are
//! appended by database triggers, so the writers cannot hand a token to the consumer directly;
//! instead every write that can append a row signals a [`Notify`] here, and the consumer waits on
//! it instead of polling on a timer. That keeps an idle process free of periodic wake-ups while a
//! write still reaches the derived data immediately.
//!
//! The signals live next to the writes, for the same reason
//! [`crate::storage::AccessChangeEpochs`] does: the storage write functions are the one place every
//! writer passes through, so a new call site cannot silently forget to signal. Triggers do not tell
//! us *which* stream they appended to, so a signal is deliberately coarse: waking a consumer that
//! finds nothing costs one bounded query pass, while a missed signal would leave derived data stale
//! until the idle fallback. Long fallbacks are therefore only a safety net for writes this process
//! cannot observe (another process, a restore from a backup file, or a future write path that
//! forgets to signal).
//!
//! Signals are coalesced by [`Notify`]: a burst of writes leaves at most one pending wake-up, and a
//! consumer that is already draining keeps its permit for the pass after it catches up.

use tokio::sync::{Notify, futures::Notified};

/// The wake-up handles shared by every `Database` clone of one process.
#[derive(Debug, Default)]
pub(crate) struct OutboxSignals {
    catalog_items: Notify,
    search_reindex: Notify,
    collections: Notify,
}

impl OutboxSignals {
    /// An authoritative metadata write committed; any of the three streams may hold new rows.
    ///
    /// Every signal is sent unconditionally: deciding which trigger fired would mean repeating the
    /// trigger predicates at the write site, and a redundant wake-up is much cheaper than a missed
    /// one.
    pub(crate) fn notify_writers(&self) {
        self.catalog_items.notify_one();
        self.search_reindex.notify_one();
        self.collections.notify_one();
    }

    /// Resolves once a write signalled the `catalog_items` consumer.
    pub(crate) fn notified_catalog_items(&self) -> Notified<'_> {
        self.catalog_items.notified()
    }

    /// Resolves once a write signalled the search reindex consumer.
    pub(crate) fn notified_search_reindex(&self) -> Notified<'_> {
        self.search_reindex.notified()
    }

    /// Resolves once a write signalled the global collection sync worker.
    pub(crate) fn notified_collections(&self) -> Notified<'_> {
        self.collections.notified()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::storage::test_database;

    /// A write that only ever queued `catalog_outbox`/`search_outbox` rows through a trigger must
    /// still wake both consumers: this is the path the read model and the search index rely on when
    /// they stop polling.
    #[tokio::test]
    async fn an_unlocked_media_item_write_signals_the_consumers() {
        let (_temp, database) = match test_database().await {
            Ok(database) => database,
            Err(error) => panic!("test database: {error}"),
        };
        sqlx::query(
            "INSERT INTO libraries (id, name, kind, is_enabled)
             VALUES ('signals-library', 'Signals', 'MOVIE', 1)",
        )
        .execute(database.pool())
        .await
        .expect("library fixture");
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 has_available_source, removed_at
             ) VALUES ('signals-item', 'signals-library', 'MOVIE', 'Signals', 'signals',
                       'LOCAL_CONFIRMED', 1, unixepoch())",
        )
        .execute(database.pool())
        .await
        .expect("item fixture");

        let signals = database.outbox_signals();
        let catalog = signals.notified_catalog_items();
        tokio::pin!(catalog);
        catalog.as_mut().enable();
        let search = signals.notified_search_reindex();
        tokio::pin!(search);
        search.as_mut().enable();

        // `restore_media_item` is one of the storage writes outside the metadata write lock, so it
        // has to signal explicitly.
        database
            .restore_media_item("signals-item")
            .await
            .expect("restore");

        tokio::time::timeout(Duration::from_secs(1), catalog)
            .await
            .expect("the catalog consumer must be signalled");
        tokio::time::timeout(Duration::from_secs(1), search)
            .await
            .expect("the search consumer must be signalled");
    }

    /// A write that holds the metadata write lock signals through the lock guard, which is dropped
    /// after the transaction commits.
    #[tokio::test]
    async fn a_locked_write_signals_the_consumers() {
        let (_temp, database) = match test_database().await {
            Ok(database) => database,
            Err(error) => panic!("test database: {error}"),
        };
        let signals = database.outbox_signals();
        let collections = signals.notified_collections();
        tokio::pin!(collections);
        collections.as_mut().enable();

        database
            .requeue_interrupted_scan_local_metadata_batches()
            .await
            .expect("locked write");

        tokio::time::timeout(Duration::from_secs(1), collections)
            .await
            .expect("the collection worker must be signalled");
    }
}
