use std::{fmt, sync::Arc, time::Duration};

use tokio::{sync::watch, task::JoinHandle};

use crate::{
    application::{
        metadata_objects::{MetadataObjectError, MetadataObjectSnapshot, MetadataObjectStore},
        nfo::local_nfo_details_from_cache_json,
        scraper::{
            ScraperError, ScraperGetRequest, ScraperItemType, ScraperProvider, ScraperResolver,
            provider_id_for_key,
        },
    },
    storage::{
        Database, GlobalMovieCollectionReference, NewCollection, StorageError, StoredEmbyCollection,
    },
};

#[derive(Clone)]
pub struct CollectionService {
    database: Database,
    scraper: ScraperProvider,
    resolver: Option<ScraperResolver>,
    metadata_objects: Option<MetadataObjectStore>,
}

impl CollectionService {
    pub fn new<T>(database: Database, scraper: T) -> Self
    where
        T: Into<ScraperProvider>,
    {
        Self {
            database,
            scraper: scraper.into(),
            resolver: None,
            metadata_objects: None,
        }
    }

    pub fn with_resolver<T>(database: Database, scraper: T, resolver: ScraperResolver) -> Self
    where
        T: Into<ScraperProvider>,
    {
        Self {
            database,
            scraper: scraper.into(),
            resolver: Some(resolver),
            metadata_objects: None,
        }
    }

    pub fn with_config_dir(mut self, config_dir: std::path::PathBuf) -> Self {
        self.metadata_objects = Some(MetadataObjectStore::new(config_dir));
        self
    }

    pub async fn refresh_for_item(
        &self,
        item_id: &str,
    ) -> Result<CollectionRefreshReport, CollectionError> {
        let Some(identity) = self.database.find_movie_identity(item_id).await? else {
            return Err(CollectionError::MovieProviderIdMissing);
        };
        let scraper = self.provider_for_item(item_id).await?;
        let provider_name = scraper.provider_key().to_owned();
        let movie = scraper
            .get_generic(ScraperGetRequest::new(
                ScraperItemType::Movie,
                identity.provider_id.clone(),
                "zh-CN",
            ))
            .await
            .map_err(CollectionError::Scraper)?;
        let Some(collection) = movie.collection else {
            return Err(CollectionError::NoCollection);
        };
        let collection_id = collection
            .provider_id
            .filter(|value| !value.trim().is_empty())
            .ok_or(CollectionError::InvalidProviderId)?;
        let details = scraper
            .get_generic(ScraperGetRequest::new(
                ScraperItemType::BoxSet,
                collection_id.clone(),
                "zh-CN",
            ))
            .await
            .map_err(CollectionError::Scraper)?;
        let selected_provider_id = provider_id_for_key(&details.provider_ids, &provider_name)
            .or_else(|| provider_id_for_key(&details.provider_ids, &identity.provider_name));
        if selected_provider_id.is_none() {
            return Err(CollectionError::InvalidProviderId);
        }
        let title = details
            .title
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Scraper Collection");
        let member_provider_ids = details
            .items
            .iter()
            .filter_map(|part| {
                let id = provider_id_for_key(&part.provider_ids, &provider_name)
                    .or_else(|| provider_id_for_key(&part.provider_ids, &identity.provider_name))?
                    .to_owned();
                Some((provider_name.clone(), id, 0))
            })
            .collect::<Vec<_>>();
        let result = self
            .database
            .upsert_collection(NewCollection {
                library_id: &identity.library_id,
                provider: &provider_name,
                provider_id: &collection_id,
                title,
                overview: details.overview.as_deref(),
                poster_path: None,
                backdrop_path: None,
                member_provider_ids: &member_provider_ids,
            })
            .await?;
        if let Some(metadata_objects) = &self.metadata_objects {
            let mut snapshot = MetadataObjectSnapshot::new(
                crate::application::metadata_paths::MetadataObjectKind::Collection,
                title,
                &provider_name,
                &collection_id,
            )?
            .with_member_count(result.member_count);
            if let Some(overview) = details.overview.clone() {
                snapshot = snapshot.with_overview(overview);
            }
            metadata_objects.write_snapshot(snapshot).await?;
        }
        Ok(CollectionRefreshReport {
            source_item_id: item_id.to_owned(),
            collection_item_id: result.collection_item_id,
            member_count: result.member_count,
        })
    }

    pub(crate) async fn create_emby_collection(
        &self,
        title: &str,
        item_ids: &[String],
    ) -> Result<Option<StoredEmbyCollection>, StorageError> {
        self.database.create_emby_collection(title, item_ids).await
    }

    pub(crate) async fn add_emby_collection_items(
        &self,
        collection_item_id: &str,
        item_ids: &[String],
    ) -> Result<Option<StoredEmbyCollection>, StorageError> {
        self.database
            .add_emby_collection_items(collection_item_id, item_ids)
            .await
    }

    pub(crate) async fn remove_emby_collection_items(
        &self,
        collection_item_id: &str,
        item_ids: &[String],
    ) -> Result<Option<StoredEmbyCollection>, StorageError> {
        self.database
            .remove_emby_collection_items(collection_item_id, item_ids)
            .await
    }

    async fn provider_for_item(&self, item_id: &str) -> Result<ScraperProvider, CollectionError> {
        let Some(resolver) = &self.resolver else {
            return Ok(self.scraper.clone());
        };
        resolver
            .for_item(item_id)
            .await
            .map_err(CollectionError::Scraper)
            .map(|client| {
                client
                    .map(ScraperProvider::from_scraper)
                    .unwrap_or_else(|| self.scraper.clone())
            })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollectionRefreshReport {
    pub source_item_id: String,
    pub collection_item_id: String,
    pub member_count: usize,
}

#[derive(Debug)]
pub enum CollectionError {
    MovieProviderIdMissing,
    InvalidProviderId,
    NoCollection,
    Scraper(ScraperError),
    Storage(StorageError),
    Metadata(MetadataObjectError),
}

impl fmt::Display for CollectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MovieProviderIdMissing => formatter.write_str("movie has no scraper provider ID"),
            Self::InvalidProviderId => formatter.write_str("scraper provider ID is invalid"),
            Self::NoCollection => formatter.write_str("movie does not belong to a collection"),
            Self::Scraper(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
            Self::Metadata(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CollectionError {}

impl From<StorageError> for CollectionError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<MetadataObjectError> for CollectionError {
    fn from(error: MetadataObjectError) -> Self {
        Self::Metadata(error)
    }
}

const GLOBAL_COLLECTION_SYNC_BATCH_SIZE: usize = 32;
const GLOBAL_COLLECTION_REFERENCE_RECONCILIATION_BATCH_SIZE: usize = 32;
/// How long a failed pass waits before retrying. Failures are local (a locked table, a scraper
/// deadline) and usually clear quickly, so this stays short.
const GLOBAL_COLLECTION_SYNC_RETRY_DELAY: Duration = Duration::from_secs(1);
/// How long an idle worker sleeps before checking the outbox on its own.
///
/// Completing a write signals the worker (see `storage::OutboxSignals`), so this is not the
/// freshness path: it only covers rows this process cannot observe, such as another process
/// sharing the database.
const GLOBAL_COLLECTION_SYNC_IDLE_FALLBACK: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct GlobalCollectionSyncWorker {
    shutdown_sender: watch::Sender<bool>,
    task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

impl GlobalCollectionSyncWorker {
    pub fn start(database: Database) -> Self {
        Self::start_with_timings(
            database,
            GLOBAL_COLLECTION_SYNC_RETRY_DELAY,
            GLOBAL_COLLECTION_SYNC_IDLE_FALLBACK,
        )
    }

    fn start_with_timings(
        database: Database,
        retry_delay: Duration,
        idle_fallback: Duration,
    ) -> Self {
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let task = tokio::spawn(run_global_collection_sync_worker(
            database,
            shutdown_receiver,
            retry_delay,
            idle_fallback,
        ));
        Self {
            shutdown_sender,
            task: Arc::new(tokio::sync::Mutex::new(Some(task))),
        }
    }

    pub async fn shutdown(&self) {
        self.shutdown_sender.send_replace(true);
        if let Some(task) = self.task.lock().await.take()
            && let Err(error) = task.await
        {
            tracing::warn!(%error, "global collection sync worker stopped unexpectedly");
        }
    }
}

async fn run_global_collection_sync_worker(
    database: Database,
    mut shutdown_receiver: watch::Receiver<bool>,
    retry_delay: Duration,
    idle_fallback: Duration,
) {
    loop {
        if *shutdown_receiver.borrow() {
            return;
        }
        let reconciled = match reconcile_pending_movie_nfo_collection_references(&database).await {
            Ok(reconciled) => reconciled,
            Err(error) => {
                tracing::warn!(%error, "failed to reconcile pending movie NFO collection identities; will retry");
                if wait_for_global_collection_sync_retry(&mut shutdown_receiver, retry_delay).await
                {
                    return;
                }
                continue;
            }
        };
        let pending = match database
            .list_pending_global_collection_syncs(GLOBAL_COLLECTION_SYNC_BATCH_SIZE)
            .await
        {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "failed to load global collection sync jobs; will retry");
                if wait_for_global_collection_sync_retry(&mut shutdown_receiver, retry_delay).await
                {
                    return;
                }
                continue;
            }
        };
        if pending.is_empty() {
            if reconciled > 0 {
                continue;
            }
            // Nothing queued: wait for a write signal, not for a timer tick.
            if wait_for_global_collection_sync_work(
                &database,
                &mut shutdown_receiver,
                idle_fallback,
            )
            .await
            {
                return;
            }
            continue;
        }

        let mut failed = false;
        for job in pending {
            if *shutdown_receiver.borrow() {
                return;
            }
            if let Err(error) = database
                .sync_global_collection(
                    &job.collection_provider,
                    &job.collection_provider_id,
                    job.generation,
                )
                .await
            {
                tracing::warn!(%error, "global collection sync failed; job remains queued for retry");
                failed = true;
                break;
            }
        }
        if failed
            && wait_for_global_collection_sync_retry(&mut shutdown_receiver, retry_delay).await
        {
            return;
        }
        // A queued batch was processed: loop again immediately so a backlog drains continuously.
    }
}

async fn reconcile_pending_movie_nfo_collection_references(
    database: &Database,
) -> Result<usize, StorageError> {
    let candidates = database
        .list_pending_movie_nfo_collection_reference_reconciliations(
            GLOBAL_COLLECTION_REFERENCE_RECONCILIATION_BATCH_SIZE,
        )
        .await?;
    let mut completed = 0;
    for candidate in &candidates {
        let details = local_nfo_details_from_cache_json(&candidate.nfo_metadata_json);
        let reference = details.as_ref().and_then(|details| {
            let provider_ids = provider_ids_for_collection_reconciliation(
                candidate.provider_ids_json.as_deref(),
                &details.provider_ids,
            );
            GlobalMovieCollectionReference::from_nfo(
                &provider_ids,
                candidate
                    .metadata_scraper_id
                    .as_deref()
                    .or(candidate.library_scraper_id.as_deref()),
                details.set_id.as_deref(),
                details.set_name.as_deref(),
                candidate.production_year,
            )
        });
        let reference_key = reference
            .as_ref()
            .map(GlobalMovieCollectionReference::identity_key)
            .unwrap_or_default();
        match database
            .commit_movie_nfo_collection_reference_reconciliation(
                candidate,
                reference.as_ref(),
                &reference_key,
            )
            .await
        {
            Ok(true) => completed += 1,
            Ok(false) => {}
            Err(error) => tracing::warn!(
                item_id = candidate.item_id,
                %error,
                "movie NFO collection identity could not be reconciled; item remains retryable"
            ),
        }
    }
    Ok(completed)
}

fn provider_ids_for_collection_reconciliation(
    current_json: Option<&str>,
    incoming: &std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    let mut provider_ids = current_json
        .and_then(|value| {
            serde_json::from_str::<std::collections::BTreeMap<String, String>>(value).ok()
        })
        .unwrap_or_default();
    for (provider, provider_id) in incoming {
        let provider = provider.trim();
        let provider_id = provider_id.trim();
        if provider.is_empty()
            || provider_id.is_empty()
            || provider_ids
                .keys()
                .any(|existing| existing.eq_ignore_ascii_case(provider))
        {
            continue;
        }
        provider_ids.insert(provider.to_ascii_lowercase(), provider_id.to_owned());
    }
    provider_ids
}

/// Waits `delay` for new work, a write signal, or shutdown; returns `true` when it must stop.
///
/// Only the idle path uses this. A write signal is not a reason to end a retry delay: the worker's
/// own failed attempt is usually followed by other writes (`WriteLockGuard::drop` signals on every
/// committed write), and honouring them would turn the retry delay into a spin.
async fn wait_for_global_collection_sync_work(
    database: &Database,
    shutdown_receiver: &mut watch::Receiver<bool>,
    delay: Duration,
) -> bool {
    if *shutdown_receiver.borrow() {
        return true;
    }
    let signals = database.outbox_signals();
    tokio::select! {
        () = signals.notified_collections() => false,
        () = tokio::time::sleep(delay) => false,
        changed = shutdown_receiver.changed() => changed.is_err() || *shutdown_receiver.borrow(),
    }
}

/// Waits `delay` before retrying a failed attempt; write signals do not shorten it.
async fn wait_for_global_collection_sync_retry(
    shutdown_receiver: &mut watch::Receiver<bool>,
    delay: Duration,
) -> bool {
    if *shutdown_receiver.borrow() {
        return true;
    }
    tokio::select! {
        () = tokio::time::sleep(delay) => false,
        changed = shutdown_receiver.changed() => changed.is_err() || *shutdown_receiver.borrow(),
    }
}

#[cfg(test)]
mod global_sync_worker_tests {
    use super::GlobalCollectionSyncWorker;
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };
    use std::{error::Error, time::Duration};
    use tokio::time::{sleep, timeout};

    /// Long enough that a pass can only be caused by a write signal.
    const LONG_FALLBACK: Duration = Duration::from_secs(60);

    async fn pending_syncs(database: &Database) -> Result<i64, Box<dyn Error>> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM collection_sync_outbox")
            .fetch_one(database.pool())
            .await?;
        Ok(count)
    }

    #[tokio::test]
    async fn an_idle_worker_does_not_query_the_database() -> Result<(), Box<dyn Error>> {
        let (_directory, database) = database_with_pending_collection().await?;
        let worker = GlobalCollectionSyncWorker::start_with_timings(
            database.clone(),
            Duration::from_secs(1),
            LONG_FALLBACK,
        );
        wait_for_completed_sync(&database).await?;
        // The queue is empty now; let the worker reach its wait before measuring.
        sleep(Duration::from_millis(200)).await;
        let before = database.query_count();
        sleep(Duration::from_millis(1_500)).await;
        assert_eq!(
            database.query_count(),
            before,
            "an idle worker must wait for a write signal instead of polling every second"
        );
        worker.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn a_committed_write_wakes_the_worker_without_the_fallback() -> Result<(), Box<dyn Error>>
    {
        let (_directory, database) = database_with_pending_collection().await?;
        let worker = GlobalCollectionSyncWorker::start_with_timings(
            database.clone(),
            Duration::from_secs(1),
            LONG_FALLBACK,
        );
        wait_for_completed_sync(&database).await?;
        sleep(Duration::from_millis(200)).await;
        assert_eq!(pending_syncs(&database).await?, 0);

        // A committed write wakes the worker. The raw `removed_at` update below stands in for a
        // writer this process cannot hook (it queues the sync through the
        // `collection_media_item_sync_au` trigger but signals nothing); the restore that follows is
        // an ordinary storage write, and its signal is what makes the worker look.
        sqlx::query("UPDATE media_items SET removed_at = unixepoch() WHERE id = 'worker-movie-a'")
            .execute(database.pool())
            .await?;
        database.restore_media_item("worker-movie-a").await?;

        timeout(Duration::from_secs(2), async {
            loop {
                if pending_syncs(&database).await.unwrap_or(1) == 0 {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the write signal must apply the queued sync long before the 60 s fallback");
        worker.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn a_backlog_is_drained_by_consecutive_passes() -> Result<(), Box<dyn Error>> {
        let (_directory, database) = database_with_pending_collection().await?;
        // Two more collections on top of the fixture's own queued job.
        for (collection_id, item_id) in [
            ("worker-series-b", "worker-movie-a"),
            ("worker-series-c", "worker-movie-b"),
        ] {
            sqlx::query(
                "INSERT INTO collection_item_references (
                     item_id, movie_provider, movie_provider_id, collection_provider,
                     collection_provider_id, collection_title
                 ) VALUES (?, 'tmdb', ?, 'tmdb', ?, 'Worker Series')",
            )
            .bind(item_id)
            .bind(item_id)
            .bind(collection_id)
            .execute(database.pool())
            .await?;
        }
        let worker = GlobalCollectionSyncWorker::start_with_timings(
            database.clone(),
            Duration::from_secs(1),
            LONG_FALLBACK,
        );
        timeout(Duration::from_secs(5), async {
            loop {
                if pending_syncs(&database).await.unwrap_or(1) == 0 {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a queued backlog must be drained without waiting for a timer");
        let collections: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM collections WHERE provider = 'tmdb' AND provider_id LIKE 'worker-series%'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(collections, 3);
        worker.shutdown().await;
        Ok(())
    }

    /// The worker's own failed attempt is followed by unrelated writes, and every committed write
    /// signals the consumers — including the failed sync's own write lock. Honouring those signals
    /// during the retry delay turns the delay into a busy retry loop.
    ///
    /// A failing trigger rolls back with its transaction, so attempts cannot be counted in a table.
    /// The test measures the database work of the same window twice, once without and once with a
    /// signal every 40 ms; with a 400 ms retry delay over 800 ms both windows cost a handful of
    /// passes (measured ~60 queries), while a signal-aware retry loop spins into thousands.
    #[tokio::test]
    async fn the_retry_delay_is_not_shortened_by_write_signals() -> Result<(), Box<dyn Error>> {
        let (_directory, database) = database_with_pending_collection().await?;
        sqlx::query(
            "CREATE TRIGGER fail_retry_delay_collection_sync
             BEFORE INSERT ON collection_items
             BEGIN
                 SELECT RAISE(ABORT, 'injected worker sync failure');
             END",
        )
        .execute(database.pool())
        .await?;
        let retry_delay = Duration::from_millis(400);
        /// Ten times the measured cost of two retry delays: generous enough for a slow machine,
        /// far below the thousands a signal-shortcutted retry loop produces.
        const RETRY_WINDOW_QUERY_CAP: usize = 600;

        async fn measure(database: &Database, retry_delay: Duration, signal: bool) -> usize {
            let worker = GlobalCollectionSyncWorker::start_with_timings(
                database.clone(),
                retry_delay,
                LONG_FALLBACK,
            );
            // Let the first attempt fail before the window starts.
            sleep(Duration::from_millis(200)).await;
            let before = database.query_count();
            for _ in 0..20 {
                if signal {
                    database.signal_outbox_consumers();
                }
                sleep(Duration::from_millis(40)).await;
            }
            let worked = database.query_count().saturating_sub(before);
            worker.shutdown().await;
            worked
        }

        let unsignalled = measure(&database, retry_delay, false).await;
        assert!(
            unsignalled <= RETRY_WINDOW_QUERY_CAP,
            "the retry delay must pace the retries even on its own; queries={unsignalled}"
        );
        let signalled = measure(&database, retry_delay, true).await;
        assert!(
            signalled <= RETRY_WINDOW_QUERY_CAP,
            "write signals must not shortcut the retry delay; queries={signalled}"
        );
        assert!(
            pending_syncs(&database).await? > 0,
            "the failing sync must still be queued for the next retry"
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_stops_the_worker_and_joins_it() -> Result<(), Box<dyn Error>> {
        let (_directory, database) = database_with_pending_collection().await?;
        let worker = GlobalCollectionSyncWorker::start_with_timings(
            database.clone(),
            Duration::from_secs(1),
            LONG_FALLBACK,
        );
        wait_for_completed_sync(&database).await?;
        timeout(Duration::from_secs(2), worker.shutdown())
            .await
            .expect("shutdown must not wait for the fallback");
        Ok(())
    }

    async fn database_with_pending_collection()
    -> Result<(tempfile::TempDir, Database), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8100".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Worker Movies", LibraryKind::Movie, false)
            .await?;
        for (item_id, movie_id, title) in [
            ("worker-movie-a", "movie-a", "Worker Movie A"),
            ("worker-movie-b", "movie-b", "Worker Movie B"),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title,
                     identification_status, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'ONLINE_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(title)
            .bind(title.to_lowercase())
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO collection_item_references (
                     item_id, movie_provider, movie_provider_id, collection_provider,
                     collection_provider_id, collection_title
                 ) VALUES (?, 'tmdb', ?, 'tmdb', 'worker-series', 'Worker Series')",
            )
            .bind(item_id)
            .bind(movie_id)
            .execute(database.pool())
            .await?;
        }
        Ok((directory, database))
    }

    async fn wait_for_completed_sync(database: &Database) -> Result<(), Box<dyn Error>> {
        timeout(Duration::from_secs(5), async {
            loop {
                let pending_count: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM collection_sync_outbox")
                        .fetch_one(database.pool())
                        .await?;
                let member_count: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM collection_items member
                     JOIN collections collection ON collection.id = member.collection_id
                     WHERE collection.provider = 'tmdb'
                       AND collection.provider_id = 'worker-series'",
                )
                .fetch_one(database.pool())
                .await?;
                if pending_count == 0 && member_count == 2 {
                    return Ok::<(), sqlx::Error>(());
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        Ok(())
    }

    #[tokio::test]
    async fn background_worker_recovers_pending_collection_sync_at_startup()
    -> Result<(), Box<dyn Error>> {
        let (_directory, database) = database_with_pending_collection().await?;
        let pending_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM collection_sync_outbox")
            .fetch_one(database.pool())
            .await?;
        assert_eq!(pending_count, 1);

        let worker = GlobalCollectionSyncWorker::start(database.clone());
        wait_for_completed_sync(&database).await?;
        worker.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn background_worker_reconciles_existing_nfo_collection_ids_while_metadata_is_pending()
    -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8100".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Pending Movie Metadata", LibraryKind::Movie, false)
            .await?;

        for (item_id, movie_id, title) in [
            ("pending-collection-a", "movie-a", "未完成电影 A"),
            ("pending-collection-b", "movie-b", "未完成电影 B"),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title,
                     provider_ids_json, metadata_scraper_id,
                     identification_status, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, ?, 'org.lux.scraper.tmdb', 'PENDING', 1)",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(title)
            .bind(title.to_lowercase())
            .bind(format!("{{\"tmdb\":\"{movie_id}\"}}"))
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO media_item_nfo_cache (item_id, json, bytes)
                 VALUES (?, '{\"setId\":\"1281825\"}', length(CAST(? AS BLOB)))",
            )
            .bind(item_id)
            .bind(r#"{"setId":"1281825"}"#)
            .execute(database.pool())
            .await?;
        }

        sqlx::query(
            "CREATE TRIGGER fail_one_pending_collection_reference
             BEFORE INSERT ON collection_item_references
             WHEN NEW.item_id = 'pending-collection-a'
             BEGIN
                 SELECT RAISE(ABORT, 'injected collection reference failure');
             END",
        )
        .execute(database.pool())
        .await?;
        let worker = GlobalCollectionSyncWorker::start(database.clone());
        timeout(Duration::from_secs(5), async {
            loop {
                let pending_reference_b: Option<String> = sqlx::query_scalar(
                    "SELECT nfo_collection_reference_key FROM media_items
                     WHERE id = 'pending-collection-b'",
                )
                .fetch_one(database.pool())
                .await?;
                let pending_reference_a: Option<String> = sqlx::query_scalar(
                    "SELECT nfo_collection_reference_key FROM media_items
                     WHERE id = 'pending-collection-a'",
                )
                .fetch_one(database.pool())
                .await?;
                let pending_syncs: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM collection_sync_outbox")
                        .fetch_one(database.pool())
                        .await?;
                if pending_reference_b.is_some()
                    && pending_reference_a.is_none()
                    && pending_syncs == 0
                {
                    return Ok::<(), sqlx::Error>(());
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        sqlx::query("DROP TRIGGER fail_one_pending_collection_reference")
            .execute(database.pool())
            .await?;
        timeout(Duration::from_secs(5), async {
            loop {
                let pending_references: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM media_items
                     WHERE id IN ('pending-collection-a', 'pending-collection-b')
                       AND nfo_collection_reference_key IS NOT NULL",
                )
                .fetch_one(database.pool())
                .await?;
                let collection_members: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM collection_items member
                     JOIN collections collection ON collection.id = member.collection_id
                     WHERE collection.provider = 'tmdb'
                       AND collection.provider_id = '1281825'",
                )
                .fetch_one(database.pool())
                .await?;
                let pending_syncs: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM collection_sync_outbox")
                        .fetch_one(database.pool())
                        .await?;
                if pending_references == 2 && collection_members == 2 && pending_syncs == 0 {
                    return Ok::<(), sqlx::Error>(());
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        let initial_collection: (String, String) = sqlx::query_as(
            "SELECT id, title FROM collections
             WHERE provider = 'tmdb' AND provider_id = '1281825'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(initial_collection.1, "合集 tmdb:1281825");

        let source_fingerprint = [7_u8; 32];
        for item_id in ["pending-collection-a", "pending-collection-b"] {
            database
                .update_media_item_nfo_metadata(
                    item_id,
                    Some(r#"{"setId":"1281825","setName":"飞驰人生（系列）"}"#),
                    Some(&source_fingerprint),
                )
                .await?;
        }
        timeout(Duration::from_secs(5), async {
            loop {
                let current_collection: Option<(String, String)> = sqlx::query_as(
                    "SELECT id, title FROM collections
                     WHERE provider = 'tmdb' AND provider_id = '1281825'",
                )
                .fetch_optional(database.pool())
                .await?;
                let pending_syncs: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM collection_sync_outbox")
                        .fetch_one(database.pool())
                        .await?;
                if current_collection
                    .as_ref()
                    .is_some_and(|(_, title)| title == "飞驰人生（系列）")
                    && pending_syncs == 0
                {
                    return Ok::<(), sqlx::Error>(());
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        let updated_collection: (String, String) = sqlx::query_as(
            "SELECT id, title FROM collections
             WHERE provider = 'tmdb' AND provider_id = '1281825'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(updated_collection.0, initial_collection.0);
        worker.shutdown().await;

        Ok(())
    }

    #[tokio::test]
    async fn background_worker_retries_failed_collection_sync() -> Result<(), Box<dyn Error>> {
        let (_directory, database) = database_with_pending_collection().await?;
        sqlx::query(
            "CREATE TRIGGER fail_worker_collection_sync
             BEFORE INSERT ON collection_items
             BEGIN
                 SELECT RAISE(ABORT, 'injected worker sync failure');
             END",
        )
        .execute(database.pool())
        .await?;
        let worker = GlobalCollectionSyncWorker::start(database.clone());
        sleep(Duration::from_millis(1_200)).await;
        let pending_after_failures: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM collection_sync_outbox")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(pending_after_failures, 1);

        sqlx::query("DROP TRIGGER fail_worker_collection_sync")
            .execute(database.pool())
            .await?;
        wait_for_completed_sync(&database).await?;
        worker.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn app_state_starts_and_stops_the_global_collection_worker() -> Result<(), Box<dyn Error>>
    {
        use crate::{
            api::AppState,
            application::setup::SetupService,
            auth::{emby::EmbyAuthService, sessions::WebAuthService},
        };

        let (directory, database) = database_with_pending_collection().await?;
        let config = Config {
            http_addr: "127.0.0.1:8100".parse()?,
            config_dir: directory.path().join("config"),
        };
        let setup = SetupService::new(database.clone())?;
        setup
            .complete("Admin", "Admin", "worker test password")
            .await?;
        let state = AppState::ready(
            config,
            database.clone(),
            setup,
            WebAuthService::new(database.clone())?,
            EmbyAuthService::new(database.clone())?,
        );
        wait_for_completed_sync(&database).await?;
        state.shutdown_background_workers().await;
        Ok(())
    }
}
