use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use super::sql_templates::SqlTemplateCache;
use super::{
    NewInvitationUserAccount, NewUserRegistrationRequest, StoredUserInvitationAcceptance,
    StoredUserRegistrationChallenge, StoredUserRegistrationRequest, UserEmailChangeCreateResult,
    UserInvitationAcceptResult, UserRegistrationApproveResult, UserRegistrationCreateResult,
    UserRegistrationRejectResult, UserRegistrationRequestPage, UserRegistrationResendResult,
    UserRegistrationStatus, UserRegistrationVerifyResult,
};
use sqlx::{
    Acquire, Any, AnyPool, Executor, Row,
    any::{AnyConnectOptions, AnyPoolOptions},
    migrate::{MigrateError, Migrator},
};
use tokio::{fs, sync::Mutex as AsyncMutex};
use uuid::Uuid;

#[path = "catalog.rs"]
mod catalog;
#[path = "database_cleanup.rs"]
mod database_cleanup;
#[path = "database_diagnostics.rs"]
mod database_diagnostics;
pub use database_diagnostics::DatabaseDiagnosticsSnapshot;
#[path = "database_maintenance.rs"]
mod database_maintenance;
pub use database_maintenance::{
    DEFAULT_MAINTENANCE_INITIAL_DELAY_SECONDS, DEFAULT_MAINTENANCE_INTERVAL_SECONDS,
    MaintenanceCandidate, MaintenanceReport, SqliteStatisticsRefresh,
};
#[path = "device_pairings.rs"]
mod device_pairings;
#[path = "emby_migration.rs"]
mod emby_migration;
#[path = "jobs.rs"]
mod jobs;
pub(crate) use jobs::StoredScanLocalMetadataBackfillPage;
// This internal contract is consumed by the immediately following LUX-534 API task.
#[allow(dead_code)]
#[path = "pipeline_activity.rs"]
mod pipeline_activity;
pub(crate) use pipeline_activity::{
    StoredLibraryPipelineActivity, StoredLibraryPipelineActivityPage, StoredPipelineStageActivity,
    StoredPipelineStageKind, StoredPipelineStatusCounts,
};
#[path = "filesystem.rs"]
mod filesystem;
#[path = "media_sources.rs"]
mod media_sources;

#[path = "library.rs"]
mod library;
#[path = "library_deletion.rs"]
mod library_deletion;
pub(crate) use library_deletion::LibraryDeletionBatch;
#[path = "library_roots.rs"]
mod library_roots;
#[path = "subtitles.rs"]
mod subtitles;

#[path = "media.rs"]
mod media;
#[path = "media_merge.rs"]
mod media_merge;
#[path = "media_merge_records.rs"]
mod media_merge_records;
pub(crate) use media_merge_records::{
    MediaMergeOrigin, StoredIdentityMergeCandidateItem, StoredIdentityMergeCandidateKey,
    StoredMediaMergeRecord, StoredMediaMergeUndo,
};
#[path = "scan_fingerprint_migration.rs"]
mod scan_fingerprint_migration;
pub(crate) use media_merge::StoredMediaMerge;
#[path = "catalog_read_model.rs"]
mod catalog_read_model;
pub(crate) use catalog_read_model::{
    CATALOG_ITEMS_INCREMENTAL_CONSUMER, CATALOG_OUTBOX_KIND, ReadModelPageQuery, ReadModelSort,
    RecentByLibraryReadModelQuery, RecentReadModelQuery,
};
#[path = "search_reindex.rs"]
mod search_reindex;
pub(crate) use search_reindex::{SEARCH_REINDEX_CONSUMER, SEARCH_REINDEX_KIND};
#[path = "mail_outbox.rs"]
mod mail_outbox;
#[path = "metadata.rs"]
mod metadata;
#[path = "migration.rs"]
mod migration;
#[path = "notifications.rs"]
mod notifications;
#[path = "people.rs"]
mod people;
#[path = "sessions.rs"]
mod sessions;
#[path = "user_mail_flows.rs"]
mod user_mail_flows;
pub(crate) use sessions::{
    NewTranscodingHistory, StoredTranscodingHistory, StoredTranscodingSessionRow,
};
#[path = "playback_history_archive.rs"]
mod playback_history_archive;
#[path = "user_series_next.rs"]
mod user_series_next;
pub(crate) use user_series_next::ResumePointerQuery;
#[path = "users.rs"]
mod users;
#[path = "watch_state_archive.rs"]
mod watch_state_archive;

// A media_sources row uses 11 bind values. Keep each backend's positive-index batches
// below its parameter limit while allowing PostgreSQL fewer round trips per transaction.
const MANIFEST_POSITIVE_INDEX_INSERT_CHUNK_SIZE: usize = 2_900;
const POSTGRES_MANIFEST_POSITIVE_INDEX_INSERT_CHUNK_SIZE: usize = 5_000;
const MANIFEST_POSITIVE_INDEX_MAX_BIND_VALUES_PER_ROW: usize = 11;
const MANIFEST_EXISTING_FILE_UPDATE_SQLITE_CHUNK_SIZE: usize = 1_800;

pub(super) fn record_manifest_storage_stage(
    phase: &'static str,
    started: Instant,
    units: usize,
    files: usize,
    directories: usize,
) {
    if tracing::enabled!(target: "lux::scan_performance", tracing::Level::DEBUG) {
        tracing::debug!(
            target: "lux::scan_performance",
            phase,
            duration_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            units = u64::try_from(units).unwrap_or(u64::MAX),
            files = u64::try_from(files).unwrap_or(u64::MAX),
            directories = u64::try_from(directories).unwrap_or(u64::MAX),
            "manifest storage stage timing"
        );
    }
}
const MANIFEST_EXISTING_FILE_UPDATE_POSTGRES_CHUNK_SIZE: usize = 3_800;
const MANIFEST_EXISTING_FILE_UPDATE_MAX_BIND_VALUES_PER_ROW: usize = 17;
const SQLITE_MAX_BIND_PARAMETERS: usize = 32_766;
const POSTGRES_MAX_BIND_PARAMETERS: usize = 65_535;
// Manifest path queries have at most a handful of fixed binds in addition to each path.
// Keep a generous safety margin below both backend parameter limits.
const MANIFEST_PATH_QUERY_SQLITE_CHUNK_SIZE: usize = 2_900;
const MANIFEST_PATH_QUERY_POSTGRES_CHUNK_SIZE: usize = 5_000;
// A media_items hierarchy row has 15 bound columns; these sizes stay below each backend limit.
const MEDIA_ITEM_HIERARCHY_SQLITE_CHUNK_SIZE: usize = 2_000;
const MEDIA_ITEM_HIERARCHY_POSTGRES_CHUNK_SIZE: usize = 4_000;
const MEDIA_ITEM_HIERARCHY_MAX_BIND_VALUES_PER_ROW: usize = 15;
pub(crate) const MANIFEST_POSTPROCESSING_TARGET_PAGE_SIZE: usize = 32_000;

pub(crate) fn manifest_positive_index_insert_chunk_size(backend: DatabaseBackend) -> usize {
    let (chunk_size, max_bind_parameters) = match backend {
        DatabaseBackend::Sqlite => (
            MANIFEST_POSITIVE_INDEX_INSERT_CHUNK_SIZE,
            SQLITE_MAX_BIND_PARAMETERS,
        ),
        DatabaseBackend::Postgres => (
            POSTGRES_MANIFEST_POSITIVE_INDEX_INSERT_CHUNK_SIZE,
            POSTGRES_MAX_BIND_PARAMETERS,
        ),
    };
    debug_assert!(
        chunk_size <= max_bind_parameters / MANIFEST_POSITIVE_INDEX_MAX_BIND_VALUES_PER_ROW
    );
    chunk_size
}

pub(crate) fn manifest_existing_file_update_chunk_size(backend: DatabaseBackend) -> usize {
    let (chunk_size, max_bind_parameters) = match backend {
        DatabaseBackend::Sqlite => (
            MANIFEST_EXISTING_FILE_UPDATE_SQLITE_CHUNK_SIZE,
            SQLITE_MAX_BIND_PARAMETERS,
        ),
        DatabaseBackend::Postgres => (
            MANIFEST_EXISTING_FILE_UPDATE_POSTGRES_CHUNK_SIZE,
            POSTGRES_MAX_BIND_PARAMETERS,
        ),
    };
    debug_assert!(
        chunk_size * MANIFEST_EXISTING_FILE_UPDATE_MAX_BIND_VALUES_PER_ROW <= max_bind_parameters
    );
    chunk_size
}

/// Maximum bind parameters SQLite/PostgreSQL accept for one prepared statement.
pub(crate) fn max_sql_bind_parameters(backend: DatabaseBackend) -> usize {
    match backend {
        DatabaseBackend::Sqlite => SQLITE_MAX_BIND_PARAMETERS,
        DatabaseBackend::Postgres => POSTGRES_MAX_BIND_PARAMETERS,
    }
}

/// Rows per media-source insert, capped by the backend's bind budget (13 binds per row).
pub(crate) fn media_source_insert_chunk_size(backend: DatabaseBackend, requested: usize) -> usize {
    const BIND_VALUES_PER_ROW: usize = 13;
    (max_sql_bind_parameters(backend) / BIND_VALUES_PER_ROW)
        .min(requested)
        .max(1)
}

pub(crate) fn manifest_path_query_chunk_size(backend: DatabaseBackend) -> usize {
    match backend {
        DatabaseBackend::Sqlite => MANIFEST_PATH_QUERY_SQLITE_CHUNK_SIZE,
        DatabaseBackend::Postgres => MANIFEST_PATH_QUERY_POSTGRES_CHUNK_SIZE,
    }
}

pub(crate) fn manifest_seen_filesystem_update_chunk_size(backend: DatabaseBackend) -> usize {
    match backend {
        // The SQLite presence path binds seven values per row when filling its
        // temporary table; keep the chunk below SQLite's variable limit instead
        // of assuming three binds per row.
        DatabaseBackend::Sqlite => SQLITE_MAX_BIND_PARAMETERS.saturating_sub(2) / 7,
        DatabaseBackend::Postgres => manifest_path_query_chunk_size(backend),
    }
}

pub(crate) fn media_item_hierarchy_insert_chunk_size(backend: DatabaseBackend) -> usize {
    let (chunk_size, max_bind_parameters) = match backend {
        DatabaseBackend::Sqlite => (
            MEDIA_ITEM_HIERARCHY_SQLITE_CHUNK_SIZE,
            SQLITE_MAX_BIND_PARAMETERS,
        ),
        DatabaseBackend::Postgres => (
            MEDIA_ITEM_HIERARCHY_POSTGRES_CHUNK_SIZE,
            POSTGRES_MAX_BIND_PARAMETERS,
        ),
    };
    debug_assert!(chunk_size * MEDIA_ITEM_HIERARCHY_MAX_BIND_VALUES_PER_ROW <= max_bind_parameters);
    chunk_size
}

#[cfg(test)]
mod manifest_positive_index_batch_tests {
    use super::{
        DatabaseBackend, MANIFEST_EXISTING_FILE_UPDATE_MAX_BIND_VALUES_PER_ROW,
        MANIFEST_POSITIVE_INDEX_MAX_BIND_VALUES_PER_ROW,
        MEDIA_ITEM_HIERARCHY_MAX_BIND_VALUES_PER_ROW, POSTGRES_MAX_BIND_PARAMETERS,
        SQLITE_MAX_BIND_PARAMETERS, manifest_existing_file_update_chunk_size,
        manifest_path_query_chunk_size, manifest_positive_index_insert_chunk_size,
        manifest_seen_filesystem_update_chunk_size, media_item_hierarchy_insert_chunk_size,
        media_source_insert_chunk_size,
    };

    #[test]
    fn backend_batches_respect_query_parameter_limits() {
        let sqlite_size = manifest_positive_index_insert_chunk_size(DatabaseBackend::Sqlite);
        let postgres_size = manifest_positive_index_insert_chunk_size(DatabaseBackend::Postgres);

        assert_eq!(sqlite_size, 2_900);
        assert_eq!(postgres_size, 5_000);
        assert!(
            sqlite_size * MANIFEST_POSITIVE_INDEX_MAX_BIND_VALUES_PER_ROW
                <= SQLITE_MAX_BIND_PARAMETERS
        );
        assert!(
            postgres_size * MANIFEST_POSITIVE_INDEX_MAX_BIND_VALUES_PER_ROW
                <= POSTGRES_MAX_BIND_PARAMETERS
        );
        assert!(postgres_size > sqlite_size);

        let sqlite_update_size = manifest_existing_file_update_chunk_size(DatabaseBackend::Sqlite);
        let postgres_update_size =
            manifest_existing_file_update_chunk_size(DatabaseBackend::Postgres);
        assert_eq!(sqlite_update_size, 1_800);
        assert_eq!(postgres_update_size, 3_800);
        assert!(
            sqlite_update_size * MANIFEST_EXISTING_FILE_UPDATE_MAX_BIND_VALUES_PER_ROW
                <= SQLITE_MAX_BIND_PARAMETERS
        );
        assert!(
            postgres_update_size * MANIFEST_EXISTING_FILE_UPDATE_MAX_BIND_VALUES_PER_ROW
                <= POSTGRES_MAX_BIND_PARAMETERS
        );

        // The media-source insert binds 13 values per row, so the 2,900-row path batch would
        // overflow SQLite's 32,766-variable limit on database scans.
        let sqlite_source_size = media_source_insert_chunk_size(DatabaseBackend::Sqlite, 2_900);
        assert_eq!(sqlite_source_size, 2_520);
        assert!(sqlite_source_size * 13 <= SQLITE_MAX_BIND_PARAMETERS);
        assert_eq!(
            media_source_insert_chunk_size(DatabaseBackend::Postgres, 2_900),
            2_900
        );
    }

    #[test]
    fn manifest_path_batches_are_backend_aware() {
        assert_eq!(
            manifest_path_query_chunk_size(DatabaseBackend::Sqlite),
            2_900
        );
        assert_eq!(
            manifest_path_query_chunk_size(DatabaseBackend::Postgres),
            5_000
        );
    }

    #[test]
    fn seen_filesystem_update_batches_use_all_safe_sqlite_parameters() {
        let sqlite_size = manifest_seen_filesystem_update_chunk_size(DatabaseBackend::Sqlite);
        let postgres_size = manifest_seen_filesystem_update_chunk_size(DatabaseBackend::Postgres);

        assert_eq!(sqlite_size, 4_680);
        assert_eq!(
            postgres_size,
            manifest_path_query_chunk_size(DatabaseBackend::Postgres)
        );
        assert!(sqlite_size * 7 + 2 <= SQLITE_MAX_BIND_PARAMETERS);
    }

    #[test]
    fn hierarchy_batches_respect_media_item_bind_limits() {
        let sqlite_size = media_item_hierarchy_insert_chunk_size(DatabaseBackend::Sqlite);
        let postgres_size = media_item_hierarchy_insert_chunk_size(DatabaseBackend::Postgres);

        assert_eq!(sqlite_size, 2_000);
        assert_eq!(postgres_size, 4_000);
        assert!(
            sqlite_size * MEDIA_ITEM_HIERARCHY_MAX_BIND_VALUES_PER_ROW
                <= SQLITE_MAX_BIND_PARAMETERS
        );
        assert!(
            postgres_size * MEDIA_ITEM_HIERARCHY_MAX_BIND_VALUES_PER_ROW
                <= POSTGRES_MAX_BIND_PARAMETERS
        );
        assert!(postgres_size > sqlite_size);
    }
}

pub use database_cleanup::DatabaseLifecycleCleanupReport;
pub(crate) use device_pairings::DevicePairingRedeemResult;

pub(crate) use emby_migration::{
    EmbyMigrationHandledItemBatch, EmbyMigrationImportRecordBatch, EmbyMigrationItemMatchBatch,
    EmbyMigrationItemPageBatch, EmbyMigrationJobProgress, EmbyMigrationPersonFavoriteBatch,
    EmbyMigrationPersonFavoriteStateBatch, EmbyMigrationUserItemStateBatch,
    EmbyMigrationUserItemStateFields, MigrationMediaIdentityLookup, MigrationPersonIdentityLookup,
    NewEmbyMigrationJob, StoredEmbyMigrationImportRecord, StoredEmbyMigrationItemMatch,
    StoredEmbyMigrationJob, StoredEmbyMigrationPersonFavorite, StoredEmbyMigrationSource,
    StoredEmbyMigrationUserBinding, StoredEmbyMigrationUserLink, StoredMigrationMediaIdentity,
    StoredMigrationPersonIdentity, StoredPlaybackHistoryEvent,
};

use crate::observability::logs::{LogStore, NewAuditLogEvent};
use crate::{
    config::{Config, DatabaseBackend, DatabaseConfiguration, DatabaseConfigurationError},
    domain::content_restrictions::{
        ContentRestrictionPolicy, content_restriction_policy_is_restrictive,
        content_restriction_policy_is_valid,
    },
};

// Keep the embedded migrator source coupled to newly added migration files.
static SQLITE_MIGRATOR: Migrator = sqlx::migrate!("./migrations");
static POSTGRES_MIGRATOR: Migrator = sqlx::migrate!("./migrations-postgres");

// SQLite migration 0126 used this checksum before the ALTER TABLE default was made compatible
// with populated databases. Accept only this exact already-applied version; new databases keep
// applying the current migration and its backfill/trigger.
const LEGACY_SQLITE_MIGRATION_0126_CHECKSUM: &[u8] = &[
    0xfb, 0x25, 0x0e, 0x26, 0x1d, 0xf5, 0x07, 0xa3, 0x2a, 0xa8, 0xca, 0xdf, 0x24, 0x7b, 0x2d, 0x69,
    0x5e, 0x0f, 0x65, 0xdf, 0x7b, 0x0f, 0xc9, 0x4f, 0x29, 0xb8, 0x7f, 0xd6, 0x38, 0x9a, 0x49, 0x8d,
    0x0f, 0xc3, 0x31, 0xf4, 0x74, 0x1e, 0x5b, 0xd2, 0xc2, 0x6b, 0x71, 0xfa, 0x67, 0x67, 0xda, 0xd6,
];

pub(crate) const PLAYBACK_SESSION_STALE_AFTER_SECONDS: i64 = 90;
pub(crate) const MAX_PLAYBACK_SESSION_WINDOW_SECONDS: i64 = 30 * 24 * 60 * 60;
pub(crate) const DEFAULT_PLAYED_PERCENT: i64 = 95;
const MAX_BACKGROUND_PAGE_SIZE: i64 = 500;
/// Sliding window for Emby access tokens; clients re-authenticate after it lapses.
pub(crate) const EMBY_ACCESS_TOKEN_TTL_SECONDS: i64 = 180 * 24 * 60 * 60;
const BATCH_INSERT_CHUNK_SIZE: usize = 100;
// Four binds per reconciliation row keep 200 rows below SQLite's historical 999-variable limit.
const SCAN_DML_CHUNK_SIZE: usize = 200;
// Eight binds per delta row keep each insert batch below SQLite's historical 999-variable limit.
const SCAN_MANIFEST_DELTA_BATCH_SIZE: usize = 100;
const RECOMMENDATION_RATING_CACHE_TTL_SECONDS: i64 = 30 * 86_400;
const DATABASE_POOL_MAX_CONNECTIONS_ENV: &str = "LUX_DB_MAX_CONNECTIONS";
const SQLITE_DATABASE_POOL_MAX_CONNECTIONS: u32 = 8;
const POSTGRES_DATABASE_POOL_MAX_CONNECTIONS: u32 = 8;
const MIN_DATABASE_POOL_MAX_CONNECTIONS: u32 = 1;
const MAX_DATABASE_POOL_MAX_CONNECTIONS: u32 = 100;

/// Opt-in switch for incremental auto-vacuum. Measured on the deletion path, keeping
/// the pointer map up to date costs about 1.7x (4.0M rows: 38 s without it, 64 s
/// with it), so it is not a default: it buys bounded space reclamation at the price
/// of a slower write path. Operators who would rather reclaim space after deleting a
/// library than keep the fastest deletes can turn it on.
const SQLITE_AUTO_VACUUM_ENV: &str = "LUX_SQLITE_AUTO_VACUUM";

fn sqlite_incremental_auto_vacuum_requested() -> bool {
    parse_sqlite_auto_vacuum_request(std::env::var(SQLITE_AUTO_VACUUM_ENV).ok().as_deref())
}

fn parse_sqlite_auto_vacuum_request(configured: Option<&str>) -> bool {
    configured
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "incremental" | "1" | "true" | "on" | "yes"
            )
        })
        .unwrap_or(false)
}

/// SQLite only honors `auto_vacuum` when it is set before the first table exists, so
/// a brand-new database reserves incremental-vacuum mode here. Incremental mode lets
/// a later large deletion return pages to the filesystem in bounded steps instead of
/// one multi-hour full `VACUUM` that needs a second copy of the file. Existing
/// databases keep their current mode: switching it requires a full `VACUUM`, which is
/// an operator decision.
async fn reserve_sqlite_incremental_vacuum(pool: &AnyPool) -> Result<(), sqlx::Error> {
    // `auto_vacuum` is connection state until a `VACUUM` folds it into the file
    // header, so both statements have to run on the same connection.
    let mut connection = pool.acquire().await?;
    // `after_connect` already switched the file to WAL, which writes the header and
    // makes `page_count` non-zero, so emptiness is measured by user tables instead.
    let user_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
    )
    .fetch_one(&mut *connection)
    .await?;
    if user_tables > 0 {
        return Ok(());
    }
    sqlx::query("PRAGMA auto_vacuum = INCREMENTAL")
        .execute(&mut *connection)
        .await?;
    sqlx::query("VACUUM").execute(&mut *connection).await?;
    Ok(())
}

/// Timeouts applied to every pool so a saturated or half-dead database cannot hang requests.
const DATABASE_POOL_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Shutdown cannot hang on a checkpoint: a log that is not truncated only costs a replay on the
/// next start, so the budget stays short and the failure is logged, never fatal.
const WAL_TRUNCATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How long a database write lock may be held before the wait becomes worth a log line. These
/// locks are only taken by writers, so an occasional wait is normal; a wait past this threshold
/// means two subsystems are queuing on the same rows and the log should name both.
const WRITE_LOCK_WAIT_LOG_THRESHOLD: Duration = Duration::from_secs(1);
const DATABASE_POOL_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
const DATABASE_POOL_MAX_LIFETIME: std::time::Duration = std::time::Duration::from_secs(1800);

/// Acquires one of the process-wide write locks, reporting a wait that stops being negligible.
///
/// The lock is deliberately not a fair queue: the point is to serialise a handful of writers, not to
/// schedule requests. What the log line adds is the answer to "why did this write take two seconds"
/// without instrumenting every caller.
/// Guard that reports write-lock holds long enough to delay maintenance and reader waits.
///
/// The caller location is captured at acquisition so a long hold names the code path without
/// logging SQL text, identifiers, or user data.
pub(crate) struct WriteLockGuard {
    _guard: tokio::sync::OwnedMutexGuard<()>,
    operation: &'static str,
    caller: &'static std::panic::Location<'static>,
    acquired_at: Instant,
}

/// How long one writer may hold a process-wide write lock before the hold is reported.
const WRITE_LOCK_HOLD_LOG_THRESHOLD: Duration = Duration::from_secs(1);

impl Drop for WriteLockGuard {
    fn drop(&mut self) {
        let held = self.acquired_at.elapsed();
        if held >= WRITE_LOCK_HOLD_LOG_THRESHOLD {
            tracing::warn!(
                operation = self.operation,
                caller = %self.caller,
                held_ms = u64::try_from(held.as_millis()).unwrap_or(u64::MAX),
                "database write lock held for a long time"
            );
        }
    }
}

async fn acquire_write_lock_after(
    lock: &Arc<AsyncMutex<()>>,
    operation: &'static str,
    threshold: Duration,
    caller: &'static std::panic::Location<'static>,
) -> WriteLockGuard {
    match tokio::time::timeout(threshold, lock.clone().lock_owned()).await {
        Ok(guard) => WriteLockGuard {
            _guard: guard,
            operation,
            caller,
            acquired_at: Instant::now(),
        },
        Err(_) => {
            let started = Instant::now();
            tracing::info!(
                operation,
                caller = %caller,
                threshold_ms = threshold.as_millis() as u64,
                "database write lock is held by another writer; waiting"
            );
            let guard = lock.clone().lock_owned().await;
            tracing::info!(
                operation,
                caller = %caller,
                waited_ms = started.elapsed().as_millis() as u64,
                "database write lock acquired after waiting"
            );
            WriteLockGuard {
                _guard: guard,
                operation,
                caller,
                acquired_at: started,
            }
        }
    }
}

/// Returns a future so `#[track_caller]` can capture the real call site (async fns ignore it).
#[track_caller]
fn acquire_write_lock(
    lock: &Arc<AsyncMutex<()>>,
    operation: &'static str,
) -> impl std::future::Future<Output = WriteLockGuard> {
    let caller = std::panic::Location::caller();
    let lock = lock.clone();
    async move {
        acquire_write_lock_after(&lock, operation, WRITE_LOCK_WAIT_LOG_THRESHOLD, caller).await
    }
}

fn database_pool_options(max_connections: u32) -> AnyPoolOptions {
    AnyPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(DATABASE_POOL_ACQUIRE_TIMEOUT)
        .idle_timeout(Some(DATABASE_POOL_IDLE_TIMEOUT))
        .max_lifetime(Some(DATABASE_POOL_MAX_LIFETIME))
}

fn resolve_database_pool_max_connections(
    backend: DatabaseBackend,
    configured: Option<&str>,
) -> Result<u32, DatabaseConfigurationError> {
    let default = match backend {
        DatabaseBackend::Sqlite => SQLITE_DATABASE_POOL_MAX_CONNECTIONS,
        DatabaseBackend::Postgres => POSTGRES_DATABASE_POOL_MAX_CONNECTIONS,
    };
    let Some(configured) = configured
        .map(str::trim)
        .filter(|configured| !configured.is_empty())
    else {
        return Ok(default);
    };
    let value = configured.parse::<u32>().map_err(|_| {
        DatabaseConfigurationError::Invalid(format!(
            "{DATABASE_POOL_MAX_CONNECTIONS_ENV} 必须是 {MIN_DATABASE_POOL_MAX_CONNECTIONS} 到 {MAX_DATABASE_POOL_MAX_CONNECTIONS} 之间的整数"
        ))
    })?;
    if !(MIN_DATABASE_POOL_MAX_CONNECTIONS..=MAX_DATABASE_POOL_MAX_CONNECTIONS).contains(&value) {
        return Err(DatabaseConfigurationError::Invalid(format!(
            "{DATABASE_POOL_MAX_CONNECTIONS_ENV} 必须是 {MIN_DATABASE_POOL_MAX_CONNECTIONS} 到 {MAX_DATABASE_POOL_MAX_CONNECTIONS} 之间的整数"
        )));
    }
    Ok(value)
}

fn database_flag(value: bool) -> i64 {
    i64::from(value)
}

fn current_unix_timestamp() -> i64 {
    crate::clock::unix_timestamp()
}

fn recommendation_rating_cache_is_fresh(now: i64, calculated_at: i64) -> bool {
    calculated_at <= now
        && now.saturating_sub(calculated_at) < RECOMMENDATION_RATING_CACHE_TTL_SECONDS
}

pub(crate) fn recommendation_batch_key_at(unix_timestamp: i64) -> i64 {
    (unix_timestamp - 2 * 60 * 60).div_euclid(86_400)
}

fn normalize_person_name(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn playback_reached_played_threshold(
    position_ticks: i64,
    duration_ticks: i64,
    played_percent: i64,
) -> bool {
    position_ticks > 0
        && duration_ticks > 0
        && i128::from(position_ticks) * 100
            >= i128::from(duration_ticks) * i128::from(played_percent.clamp(1, 100))
}

#[derive(Clone)]
pub struct Database {
    pool: AnyPool,
    log_store: LogStore,
    pool_max_connections: u32,
    path: PathBuf,
    server_id: String,
    backend: DatabaseBackend,
    person_credits_write_lock: Arc<AsyncMutex<()>>,
    metadata_write_lock: Arc<AsyncMutex<()>>,
    recommendation_stats_refresh_lock: Arc<AsyncMutex<()>>,
    statistics_refresh_lock: Arc<AsyncMutex<()>>,
    recommendation_rating_median_cache: Arc<AsyncMutex<RecommendationRatingMedianCache>>,
    dashboard_stats_cache: Arc<AsyncMutex<Option<CachedDashboardStats>>>,
    write_probe: Arc<AsyncMutex<Option<std::time::Instant>>>,
    filesystem_path_templates: Arc<SqlTemplateCache>,
    #[cfg(test)]
    query_count: Arc<AtomicUsize>,
}

#[derive(Default)]
struct RecommendationRatingMedianCache {
    values: HashMap<Vec<String>, RecommendationRatingMedianEntry>,
}

struct RecommendationRatingMedianEntry {
    value: f64,
    calculated_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DatabasePoolSnapshot {
    pub max_connections: u32,
    pub size: u32,
    pub idle: u32,
    pub in_use: u32,
    pub saturated: bool,
}

// LUX-783 constructs these templates in the delivery worker.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MailOutboxTemplateKind {
    Invitation,
    EmailVerification,
}

#[allow(dead_code)]
impl MailOutboxTemplateKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Invitation => "INVITATION",
            Self::EmailVerification => "EMAIL_VERIFICATION",
        }
    }

    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "INVITATION" => Ok(Self::Invitation),
            "EMAIL_VERIFICATION" => Ok(Self::EmailVerification),
            _ => Err(StorageError::Serialization(
                "unknown mail template kind".to_owned(),
            )),
        }
    }
}

// LUX-783 and LUX-784 consume these states in delivery and queue administration.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MailOutboxStatus {
    Pending,
    Sending,
    RetryWait,
    Sent,
    Failed,
}

#[allow(dead_code)]
impl MailOutboxStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Sending => "SENDING",
            Self::RetryWait => "RETRY_WAIT",
            Self::Sent => "SENT",
            Self::Failed => "FAILED",
        }
    }

    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "PENDING" => Ok(Self::Pending),
            "SENDING" => Ok(Self::Sending),
            "RETRY_WAIT" => Ok(Self::RetryWait),
            "SENT" => Ok(Self::Sent),
            "FAILED" => Ok(Self::Failed),
            _ => Err(StorageError::Serialization(
                "unknown mail outbox status".to_owned(),
            )),
        }
    }
}

// LUX-783 maps transport failures to these fixed, non-sensitive values.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MailDeliveryErrorCode {
    SmtpUnavailable,
    SettingsMissing,
    TemplateMissing,
    ReferenceMissing,
}

#[allow(dead_code)]
impl MailDeliveryErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::SmtpUnavailable => "SMTP_UNAVAILABLE",
            Self::SettingsMissing => "SETTINGS_UNAVAILABLE",
            Self::TemplateMissing => "TEMPLATE_UNAVAILABLE",
            Self::ReferenceMissing => "REFERENCE_UNAVAILABLE",
        }
    }

    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "SMTP_UNAVAILABLE" => Ok(Self::SmtpUnavailable),
            "SETTINGS_UNAVAILABLE" => Ok(Self::SettingsMissing),
            "TEMPLATE_UNAVAILABLE" => Ok(Self::TemplateMissing),
            "REFERENCE_UNAVAILABLE" => Ok(Self::ReferenceMissing),
            _ => Err(StorageError::Serialization(
                "unknown mail delivery error code".to_owned(),
            )),
        }
    }
}

pub(crate) struct NewMailOutboxMessage<'a> {
    pub(crate) idempotency_key: &'a str,
    pub(crate) recipient_email: &'a str,
    pub(crate) template_kind: MailOutboxTemplateKind,
    pub(crate) reference_id: &'a str,
    pub(crate) available_at: i64,
}

// LUX-783 consumes the delivery fields and LUX-784 exposes the safe status fields.
#[allow(dead_code)]
pub(crate) struct StoredMailOutboxMessage {
    pub(crate) id: String,
    pub(crate) idempotency_key: String,
    pub(crate) recipient_email: String,
    pub(crate) template_kind: MailOutboxTemplateKind,
    pub(crate) reference_id: String,
    pub(crate) status: MailOutboxStatus,
    pub(crate) attempt_count: i64,
    pub(crate) available_at: i64,
    pub(crate) lease_expires_at: Option<i64>,
    pub(crate) last_error_code: Option<MailDeliveryErrorCode>,
    pub(crate) sent_at: Option<i64>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

// LUX-784 maps this bounded page to the admin queue API.
#[allow(dead_code)]
pub(crate) struct MailOutboxPage {
    pub(crate) messages: Vec<StoredMailOutboxMessage>,
    pub(crate) total_count: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // LUX-789 consumes these states in the invitation API.
pub(crate) enum UserInvitationStatus {
    Pending,
    Accepted,
    Revoked,
    Expired,
}

impl UserInvitationStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Accepted => "ACCEPTED",
            Self::Revoked => "REVOKED",
            Self::Expired => "EXPIRED",
        }
    }

    #[allow(dead_code)] // LUX-789 parses persisted states in the invitation API.
    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "PENDING" => Ok(Self::Pending),
            "ACCEPTED" => Ok(Self::Accepted),
            "REVOKED" => Ok(Self::Revoked),
            "EXPIRED" => Ok(Self::Expired),
            _ => Err(StorageError::Serialization(
                "unknown user invitation status".to_owned(),
            )),
        }
    }
}

pub(crate) struct NewUserInvitation<'a> {
    pub(crate) id: &'a str,
    pub(crate) recipient_email: &'a str,
    pub(crate) created_by_user_id: Option<&'a str>,
    pub(crate) preset_id: &'a str,
    pub(crate) preset_name: &'a str,
    pub(crate) preset_version: i64,
    pub(crate) permission_snapshot_json: &'a str,
    pub(crate) expires_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserInvitationCreateResult {
    Created,
    AlreadyPending,
    RateLimited,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserInvitationResendResult {
    Queued,
    NotResendable,
    RateLimited,
}

const INVITATION_EMAIL_RATE_LIMIT_PER_HOUR: i64 = 5;
const INVITATION_EMAIL_RATE_WINDOW_SECONDS: i64 = 60 * 60;

#[allow(dead_code)] // LUX-789 consumes the full invitation profile in its admin API.
pub(crate) struct StoredUserInvitation {
    pub(crate) id: String,
    pub(crate) recipient_email: String,
    pub(crate) created_by_user_id: Option<String>,
    pub(crate) accepted_user_id: Option<String>,
    pub(crate) status: UserInvitationStatus,
    pub(crate) preset_id: String,
    pub(crate) preset_name: String,
    pub(crate) preset_version: i64,
    pub(crate) permission_snapshot_json: String,
    pub(crate) expires_at: i64,
    pub(crate) accepted_at: Option<i64>,
    pub(crate) revoked_at: Option<i64>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

#[allow(dead_code)] // LUX-789 returns this page from the invitation API.
pub(crate) struct UserInvitationPage {
    pub(crate) entries: Vec<StoredUserInvitation>,
    pub(crate) page_size: i64,
    pub(crate) total_count: i64,
}

// LUX-784 uses this type to rebuild a stable, non-persisted mail link token.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MailChallengeKind {
    Invitation,
    EmailVerification,
}

#[allow(dead_code)]
impl MailChallengeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Invitation => "INVITATION",
            Self::EmailVerification => "EMAIL_VERIFICATION",
        }
    }

    fn parse(value: &str) -> Result<Self, StorageError> {
        match value {
            "INVITATION" => Ok(Self::Invitation),
            "EMAIL_VERIFICATION" => Ok(Self::EmailVerification),
            _ => Err(StorageError::Serialization(
                "unknown mail challenge kind".to_owned(),
            )),
        }
    }
}

pub(crate) struct NewMailChallenge<'a> {
    pub(crate) id: &'a str,
    pub(crate) kind: MailChallengeKind,
    pub(crate) recipient_email: &'a str,
    pub(crate) reference_id: &'a str,
    pub(crate) token_hash: &'a [u8],
    pub(crate) expires_at: i64,
}

// The digest is internal storage state and must never be mapped into an API DTO.
#[allow(dead_code)]
pub(crate) struct StoredMailChallenge {
    pub(crate) id: String,
    pub(crate) kind: MailChallengeKind,
    pub(crate) recipient_email: String,
    pub(crate) reference_id: String,
    pub(crate) token_hash: Vec<u8>,
    pub(crate) expires_at: i64,
    pub(crate) consumed_at: Option<i64>,
    pub(crate) revoked_at: Option<i64>,
    pub(crate) created_at: i64,
}

#[allow(dead_code)]
#[derive(sqlx::FromRow)]
struct MailChallengeRecord {
    id: String,
    challenge_kind: String,
    recipient_email: String,
    reference_id: String,
    token_hash: Vec<u8>,
    expires_at: i64,
    consumed_at: Option<i64>,
    revoked_at: Option<i64>,
    created_at: i64,
}

impl TryFrom<MailChallengeRecord> for StoredMailChallenge {
    type Error = StorageError;

    fn try_from(record: MailChallengeRecord) -> Result<Self, Self::Error> {
        if record.token_hash.len() != 32 {
            return Err(StorageError::Serialization(
                "mail challenge digest has invalid length".to_owned(),
            ));
        }
        Ok(Self {
            id: record.id,
            kind: MailChallengeKind::parse(&record.challenge_kind)?,
            recipient_email: record.recipient_email,
            reference_id: record.reference_id,
            token_hash: record.token_hash,
            expires_at: record.expires_at,
            consumed_at: record.consumed_at,
            revoked_at: record.revoked_at,
            created_at: record.created_at,
        })
    }
}

#[allow(dead_code)]
#[derive(sqlx::FromRow)]
struct MailOutboxRecord {
    id: String,
    idempotency_key: String,
    recipient_email: String,
    template_kind: String,
    reference_id: String,
    status: String,
    attempt_count: i64,
    available_at: i64,
    lease_expires_at: Option<i64>,
    last_error_code: Option<String>,
    sent_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

#[derive(sqlx::FromRow)]
#[allow(dead_code)] // LUX-789 consumes this record in the invitation API.
struct UserInvitationRecord {
    id: String,
    recipient_email: String,
    created_by_user_id: Option<String>,
    accepted_user_id: Option<String>,
    status: String,
    preset_id: String,
    preset_name: String,
    preset_version: i64,
    permission_snapshot_json: String,
    expires_at: i64,
    accepted_at: Option<i64>,
    revoked_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

#[derive(sqlx::FromRow)]
struct UserRegistrationRequestRecord {
    recipient_email: String,
    username_normalized: String,
    display_name: String,
    password_hash: Option<String>,
    email_verified_at: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct UserRegistrationRequestSummaryRecord {
    id: String,
    recipient_email: String,
    username_normalized: String,
    display_name: String,
    permission_snapshot_json: String,
    status: String,
    expires_at: i64,
    email_verified_at: Option<i64>,
    reviewed_at: Option<i64>,
    reviewed_by_user_id: Option<String>,
    approved_user_id: Option<String>,
    last_error_code: Option<String>,
    created_at: i64,
    updated_at: i64,
}

const USER_REGISTRATION_MAIL_RATE_LIMIT_PER_HOUR: i64 = 5;
const USER_REGISTRATION_MAIL_RATE_WINDOW_SECONDS: i64 = 60 * 60;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UserInvitationPermissionSnapshot {
    is_admin: bool,
    can_manage_server: bool,
    permissions: UserInvitationPermissionSnapshotFields,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UserInvitationPermissionSnapshotFields {
    library_access_mode: String,
    library_ids: Vec<String>,
    can_download: bool,
    can_play: bool,
    max_simultaneous_playbacks: i64,
    allow_remux: bool,
    allow_audio_transcode: bool,
    allow_video_transcode: bool,
}

impl TryFrom<MailOutboxRecord> for StoredMailOutboxMessage {
    type Error = StorageError;

    fn try_from(record: MailOutboxRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.id,
            idempotency_key: record.idempotency_key,
            recipient_email: record.recipient_email,
            template_kind: MailOutboxTemplateKind::parse(&record.template_kind)?,
            reference_id: record.reference_id,
            status: MailOutboxStatus::parse(&record.status)?,
            attempt_count: record.attempt_count,
            available_at: record.available_at,
            lease_expires_at: record.lease_expires_at,
            last_error_code: record
                .last_error_code
                .as_deref()
                .map(MailDeliveryErrorCode::parse)
                .transpose()?,
            sent_at: record.sent_at,
            created_at: record.created_at,
            updated_at: record.updated_at,
        })
    }
}

impl TryFrom<UserInvitationRecord> for StoredUserInvitation {
    type Error = StorageError;

    fn try_from(record: UserInvitationRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.id,
            recipient_email: record.recipient_email,
            created_by_user_id: record.created_by_user_id,
            accepted_user_id: record.accepted_user_id,
            status: UserInvitationStatus::parse(&record.status)?,
            preset_id: record.preset_id,
            preset_name: record.preset_name,
            preset_version: record.preset_version,
            permission_snapshot_json: record.permission_snapshot_json,
            expires_at: record.expires_at,
            accepted_at: record.accepted_at,
            revoked_at: record.revoked_at,
            created_at: record.created_at,
            updated_at: record.updated_at,
        })
    }
}

impl Database {
    pub async fn connect(config: &Config) -> Result<Self, StorageError> {
        Self::connect_with_configuration(config, &DatabaseConfiguration::Sqlite).await
    }

    pub async fn connect_with_configuration(
        config: &Config,
        configuration: &DatabaseConfiguration,
    ) -> Result<Self, StorageError> {
        configuration
            .validate()
            .map_err(StorageError::Configuration)?;
        let backend = configuration.backend();
        let configured_max_connections = std::env::var(DATABASE_POOL_MAX_CONNECTIONS_ENV).ok();
        let pool_max_connections =
            resolve_database_pool_max_connections(backend, configured_max_connections.as_deref())
                .map_err(StorageError::Configuration)?;
        fs::create_dir_all(&config.config_dir)
            .await
            .map_err(|source| StorageError::Io {
                path: config.config_dir.clone(),
                source,
            })?;

        let path = match backend {
            DatabaseBackend::Sqlite => config.config_dir.join("lux.db"),
            DatabaseBackend::Postgres => PathBuf::from("external PostgreSQL"),
        };
        sqlx::any::install_default_drivers();
        let database_url = match configuration {
            DatabaseConfiguration::Sqlite => format!("sqlite://{}?mode=rwc", path.display()),
            DatabaseConfiguration::Postgres(_) => configuration
                .postgres_url()
                .map_err(StorageError::Configuration)?
                .ok_or_else(|| {
                    StorageError::Configuration(DatabaseConfigurationError::Invalid(
                        "PostgreSQL 连接配置缺失".to_owned(),
                    ))
                })?,
        };
        migration::refuse_empty_database_when_initialized(
            &config.config_dir,
            &path,
            backend,
            &database_url,
        )
        .await?;
        let options =
            AnyConnectOptions::from_str(&database_url).map_err(|source| StorageError::Sqlx {
                path: path.clone(),
                source,
            })?;
        let after_connect_sql = match backend {
            DatabaseBackend::Sqlite => {
                "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;
                 PRAGMA busy_timeout = 5000;
                 PRAGMA journal_size_limit = 67108864;"
            }
            DatabaseBackend::Postgres => {
                "SET TIME ZONE 'UTC';
                 SET idle_in_transaction_session_timeout = '60s';
                 SET work_mem = '16MB';
                 SET jit = off;
                 SET max_parallel_workers_per_gather = 1;
                 SET statement_timeout = '1800s'"
            }
        };
        let pool = database_pool_options(pool_max_connections)
            .after_connect(move |connection, _| {
                Box::pin(async move {
                    connection.execute(after_connect_sql).await?;
                    if backend == DatabaseBackend::Postgres {
                        connection.execute("SET application_name = 'lux'").await?;
                    }
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: path.clone(),
                source,
            })?;
        if backend == DatabaseBackend::Sqlite && sqlite_incremental_auto_vacuum_requested() {
            reserve_sqlite_incremental_vacuum(&pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: path.clone(),
                    source,
                })?;
        }

        if backend == DatabaseBackend::Postgres
            && let Err(error) = migration::validate_postgres_schema(&pool).await
        {
            pool.close().await;
            return Err(error);
        }

        let migration_result = match backend {
            DatabaseBackend::Sqlite => {
                match sqlite_has_legacy_timestamp_migration_checksum(&pool).await {
                    Ok(true) => {
                        if let Some(legacy_migrator) =
                            sqlite_migrator_with_legacy_timestamp_checksum()
                        {
                            legacy_migrator.run(&pool).await
                        } else {
                            SQLITE_MIGRATOR.run(&pool).await
                        }
                    }
                    Ok(false) => SQLITE_MIGRATOR.run(&pool).await,
                    Err(source) => Err(MigrateError::Execute(source)),
                }
            }
            DatabaseBackend::Postgres => POSTGRES_MIGRATOR.run(&pool).await,
        };
        if let Err(source) = migration_result {
            pool.close().await;
            return Err(StorageError::Migration { path, source });
        }
        if backend == DatabaseBackend::Sqlite {
            migration::migrate_sqlite_catalog_constraints(&pool, &path).await?;
        }
        let server_id = migration::ensure_server_id(&pool, backend)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: path.clone(),
                source,
            })?;
        migration::record_server_state(&config.config_dir, &server_id).await?;

        Ok(Self {
            pool,
            log_store: LogStore::new(&config.config_dir),
            pool_max_connections,
            path,
            server_id,
            backend,
            person_credits_write_lock: Arc::new(AsyncMutex::new(())),
            metadata_write_lock: Arc::new(AsyncMutex::new(())),
            recommendation_stats_refresh_lock: Arc::new(AsyncMutex::new(())),
            statistics_refresh_lock: Arc::new(AsyncMutex::new(())),
            recommendation_rating_median_cache: Arc::new(AsyncMutex::new(
                RecommendationRatingMedianCache::default(),
            )),
            dashboard_stats_cache: Arc::new(AsyncMutex::new(None)),
            write_probe: Arc::new(AsyncMutex::new(None)),
            filesystem_path_templates: Arc::new(SqlTemplateCache::default()),
            #[cfg(test)]
            query_count: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Rolls a failed write back, logging a rollback failure instead of discarding it.
    ///
    /// Callers must be finished with the transaction; it is consumed here.
    pub(crate) async fn rollback_write_transaction(
        transaction: sqlx::Transaction<'_, sqlx::Any>,
        operation: &'static str,
    ) {
        if let Err(error) = transaction.rollback().await {
            tracing::warn!(%error, operation, "failed to roll back a write transaction");
        }
    }

    #[track_caller]
    pub(crate) fn acquire_metadata_write_lock(
        &self,
    ) -> impl std::future::Future<Output = Option<WriteLockGuard>> {
        let caller = std::panic::Location::caller();
        let backend = self.backend;
        let lock = self.metadata_write_lock.clone();
        async move {
            if backend == DatabaseBackend::Sqlite {
                Some(
                    acquire_write_lock_after(
                        &lock,
                        "metadata_write",
                        WRITE_LOCK_WAIT_LOG_THRESHOLD,
                        caller,
                    )
                    .await,
                )
            } else {
                None
            }
        }
    }

    /// Starts a SQLite write transaction that may read before it writes.
    ///
    /// A deferred transaction upgrades to the write lock on its first write; when another
    /// connection committed in between, that upgrade fails with `SQLITE_BUSY_SNAPSHOT` and
    /// `busy_timeout` does not help. Any transaction that reads a row it later updates must
    /// start here. Transactions whose first statement is already a write keep the cheaper
    /// deferred `begin()`.
    async fn begin_metadata_write_transaction(
        &self,
    ) -> Result<sqlx::Transaction<'_, Any>, StorageError> {
        let transaction = if self.backend == DatabaseBackend::Sqlite {
            self.pool.begin_with("BEGIN IMMEDIATE").await
        } else {
            self.pool.begin().await
        };
        transaction.map_err(self.sqlx_err())
    }

    // LUX-620 consumes this transaction helper through the admission methods in `sessions`.
    #[allow(dead_code)]
    pub(crate) async fn begin_playback_admission_transaction(
        &self,
    ) -> Result<sqlx::Transaction<'_, Any>, StorageError> {
        let transaction = if self.backend == DatabaseBackend::Sqlite {
            self.pool.begin_with("BEGIN IMMEDIATE").await
        } else {
            self.pool.begin().await
        };
        transaction.map_err(self.sqlx_err())
    }

    pub(crate) async fn begin_scan_write_transaction(
        &self,
    ) -> Result<sqlx::Transaction<'_, Any>, StorageError> {
        let transaction = if self.backend == DatabaseBackend::Sqlite {
            self.pool.begin_with("BEGIN IMMEDIATE").await
        } else {
            self.pool.begin().await
        };
        let mut transaction = transaction.map_err(self.sqlx_err())?;
        if self.backend == DatabaseBackend::Postgres {
            // Keep the reduced commit durability limited to this rebuildable scan transaction.
            sqlx::query("SET LOCAL synchronous_commit = off")
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
        }
        Ok(transaction)
    }

    pub(crate) async fn lock_scan_fingerprint_migration_state(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "INSERT INTO scan_fingerprint_migration_state (library_id)
             VALUES (?) ON CONFLICT (library_id) DO NOTHING",
        )
        .bind(library_id)
        .execute(&mut **transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.query(
            "UPDATE scan_fingerprint_migration_state SET updated_at = updated_at
             WHERE library_id = ?",
        )
        .bind(library_id)
        .execute(&mut **transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.query_scalar(
            "SELECT CASE WHEN lease_token IS NOT NULL
                              AND lease_expires_at > unixepoch()
                         THEN 1 ELSE 0 END
             FROM scan_fingerprint_migration_state WHERE library_id = ?",
        )
        .bind(library_id)
        .fetch_one(&mut **transaction)
        .await
        .map(|value: i64| value == 0)
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn lock_media_items_for_update(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        item_ids: &[String],
    ) -> Result<(), StorageError> {
        if self.backend != DatabaseBackend::Postgres || item_ids.is_empty() {
            return Ok(());
        }
        let mut unique_ids = item_ids.to_vec();
        unique_ids.sort_unstable();
        unique_ids.dedup();
        for chunk in unique_ids.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id FROM media_items
                 WHERE id IN ({placeholders}) ORDER BY id FOR UPDATE"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in chunk {
                statement = statement.bind(item_id);
            }
            statement
                .fetch_all(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
        }
        Ok(())
    }

    pub async fn test_configuration(
        configuration: &DatabaseConfiguration,
    ) -> Result<(), StorageError> {
        configuration
            .validate()
            .map_err(StorageError::Configuration)?;
        if configuration.backend() == DatabaseBackend::Sqlite {
            return Ok(());
        }

        sqlx::any::install_default_drivers();
        let database_url = configuration
            .postgres_url()
            .map_err(StorageError::Configuration)?
            .ok_or_else(|| {
                StorageError::Configuration(DatabaseConfigurationError::Invalid(
                    "PostgreSQL 连接配置缺失".to_owned(),
                ))
            })?;
        let options =
            AnyConnectOptions::from_str(&database_url).map_err(|source| StorageError::Sqlx {
                path: PathBuf::from("external PostgreSQL"),
                source,
            })?;
        let pool = database_pool_options(1)
            .after_connect(|connection, _| {
                Box::pin(async move {
                    connection.execute("SET TIME ZONE 'UTC'").await?;
                    connection.execute("SET application_name = 'lux'").await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: PathBuf::from("external PostgreSQL"),
                source,
            })?;
        migration::validate_postgres_schema(&pool).await?;
        sqlx::query_scalar::<_, i64>("SELECT 1")
            .fetch_one(&pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: PathBuf::from("external PostgreSQL"),
                source,
            })?;
        pool.close().await;
        Ok(())
    }

    pub fn pool(&self) -> &AnyPool {
        &self.pool
    }

    pub(crate) fn log_store(&self) -> LogStore {
        self.log_store.clone()
    }

    pub(crate) fn pool_snapshot(&self) -> DatabasePoolSnapshot {
        let size = self.pool.size();
        let idle = self.pool.num_idle().min(size as usize) as u32;
        let in_use = size.saturating_sub(idle);
        DatabasePoolSnapshot {
            max_connections: self.pool_max_connections,
            size,
            idle,
            in_use,
            saturated: size >= self.pool_max_connections && idle == 0,
        }
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub fn backend(&self) -> DatabaseBackend {
        self.backend
    }

    fn scalar_max_function(&self) -> &'static str {
        match self.backend {
            DatabaseBackend::Sqlite => "MAX",
            DatabaseBackend::Postgres => "GREATEST",
        }
    }

    fn scalar_min_function(&self) -> &'static str {
        match self.backend {
            DatabaseBackend::Sqlite => "MIN",
            DatabaseBackend::Postgres => "LEAST",
        }
    }

    pub(crate) async fn recommendation_rating_median(
        &self,
        library_ids: &[String],
    ) -> Result<f64, StorageError> {
        let mut cache_key = library_ids.to_vec();
        cache_key.sort_unstable();
        cache_key.dedup();
        let now = current_unix_timestamp();
        {
            // Only the map lookup happens under the lock; the queries below run without it so a
            // slow database does not serialize unrelated cache readers.
            let cache = self.recommendation_rating_median_cache.lock().await;
            if let Some(entry) = cache.values.get(&cache_key)
                && recommendation_rating_cache_is_fresh(now, entry.calculated_at)
            {
                return Ok(entry.value);
            }
        }

        let persistent_key = cache_key.join("\u{001f}");
        if let Some(row) = self
            .query(
                "SELECT median_rating, calculated_at
                 FROM recommendation_rating_cache
                 WHERE cache_key = ?",
            )
            .bind(&persistent_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(self.sqlx_err())?
        {
            let calculated_at: i64 = row.get("calculated_at");
            if recommendation_rating_cache_is_fresh(now, calculated_at) {
                let median: f64 = row.get("median_rating");
                self.recommendation_rating_median_cache
                    .lock()
                    .await
                    .values
                    .insert(
                        cache_key,
                        RecommendationRatingMedianEntry {
                            value: median,
                            calculated_at,
                        },
                    );
                return Ok(median);
            }
        }

        let placeholders = std::iter::repeat_n("?", cache_key.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT COALESCE(AVG(rating), 0.0)
             FROM (
                 SELECT mi.rating,
                        ROW_NUMBER() OVER (ORDER BY mi.rating, mi.id) AS rating_rank,
                        COUNT(*) OVER () AS rating_count
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                   AND mi.item_type IN ('MOVIE', 'SERIES')
                   AND mi.library_id IN ({placeholders})
                   AND mi.rating IS NOT NULL
             ) AS rating_values
             WHERE rating_rank IN ((rating_count + 1) / 2, (rating_count + 2) / 2)"
        );
        let mut statement = self.query_scalar::<f64>(sqlx::AssertSqlSafe(query));
        for library_id in &cache_key {
            statement = statement.bind(library_id);
        }
        let median = statement
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let calculated_at = current_unix_timestamp();
        self.query(
            "INSERT INTO recommendation_rating_cache
                 (cache_key, median_rating, calculated_at)
             VALUES (?, ?, ?)
             ON CONFLICT(cache_key) DO UPDATE SET
                 median_rating = excluded.median_rating,
                 calculated_at = excluded.calculated_at",
        )
        .bind(&persistent_key)
        .bind(median)
        .bind(calculated_at)
        .execute(&self.pool)
        .await
        .map_err(self.sqlx_err())?;
        self.recommendation_rating_median_cache
            .lock()
            .await
            .values
            .insert(
                cache_key,
                RecommendationRatingMedianEntry {
                    value: median,
                    calculated_at,
                },
            );
        Ok(median)
    }

    pub async fn schema_version(&self) -> Result<i64, StorageError> {
        self.query("SELECT COALESCE(MAX(version), 0) AS version FROM _sqlx_migrations")
            .fetch_one(&self.pool)
            .await
            .map(|row| row.get::<i64, _>("version"))
            .map_err(self.sqlx_err())
    }

    pub(crate) async fn identity_stability_repair_completed(&self) -> Result<bool, StorageError> {
        self.query_scalar::<String>(
            "SELECT value FROM lux_meta WHERE key = 'identity_stability_repair_v1'",
        )
        .fetch_optional(&self.pool)
        .await
        .map(|value| value.as_deref() == Some("completed"))
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn mark_identity_stability_repair_completed(
        &self,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO lux_meta (key, value)
             VALUES ('identity_stability_repair_v1', 'completed')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    /// Verifies that SQLite can commit a real write transaction.
    ///
    /// The probe only changes a reserved metadata key and never touches
    /// application data or the schema. Committing is intentional: a rollback
    /// can succeed even when the filesystem cannot persist a durable write.
    pub async fn probe_write(&self) -> Result<(), StorageError> {
        /// Successful probes are cached so a k8s/LB liveness check does not turn into a write
        /// on every poll. Failures are never cached: readiness must notice a read-only mount
        /// immediately.
        const SUCCESS_TTL: std::time::Duration = std::time::Duration::from_secs(60);
        let last_success = self.write_probe.lock().await;
        if last_success.is_some_and(|checked_at| checked_at.elapsed() < SUCCESS_TTL) {
            return Ok(());
        }
        drop(last_success);
        let result = self.probe_write_once().await;
        if result.is_ok() {
            *self.write_probe.lock().await = Some(std::time::Instant::now());
        }
        result
    }

    async fn probe_write_once(&self) -> Result<(), StorageError> {
        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
        self.query(
            "INSERT INTO lux_meta (key, value)
             VALUES ('__lux_write_probe__', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(format!("lux-write-probe-{}", Uuid::now_v7()))
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())
    }

    /// Truncates the write-ahead log, then retires the pool.
    ///
    /// After this returns, `lux.db` alone is the whole database, which is what an operator
    /// copying the file expects and what keeps the next start from replaying a log left behind by
    /// an ungraceful stop. Nothing may be reading while the truncation runs -- a reader pins the
    /// log open and makes it fail -- so this belongs at shutdown, after the workers stopped.
    pub async fn close(self) {
        if self.backend == DatabaseBackend::Sqlite {
            match tokio::time::timeout(
                WAL_TRUNCATE_TIMEOUT,
                self.query("PRAGMA wal_checkpoint(TRUNCATE)")
                    .fetch_optional(&self.pool),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "could not truncate the write-ahead log before closing");
                }
                Err(_) => {
                    tracing::warn!("truncating the write-ahead log timed out before closing");
                }
            }
        }
        self.pool.close().await;
    }

    #[cfg(test)]
    pub(crate) fn reset_query_count(&self) {
        self.query_count.store(0, AtomicOrdering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn query_count(&self) -> usize {
        self.query_count.load(AtomicOrdering::Relaxed)
    }

    /// Maps an sqlx failure onto a storage error that names the database it came from.
    ///
    /// Shared by the modules that archive and reattach detached user state, so every one of those
    /// paths reports failures the same way.
    pub(super) fn storage_error(&self, source: sqlx::Error) -> StorageError {
        StorageError::Sqlx {
            path: self.path.clone(),
            source,
        }
    }

    /// Borrows [`Database::storage_error`] as a reusable `map_err` argument.
    ///
    /// Call sites read `.map_err(database.sqlx_err())` instead of spelling out the
    /// `StorageError::Sqlx` literal. The returned closure only borrows `self`, so nothing is
    /// allocated up front and the database path is still cloned on the error path alone.
    pub(super) fn sqlx_err(&self) -> impl Fn(sqlx::Error) -> StorageError + '_ {
        move |source| self.storage_error(source)
    }

    fn query(
        &self,
        sql: impl AdaptSqlText,
    ) -> sqlx::query::Query<'static, sqlx::Any, sqlx::any::AnyArguments> {
        #[cfg(test)]
        self.query_count.fetch_add(1, AtomicOrdering::Relaxed);
        sqlx::query(adapt_sql_for_backend(self.backend, sql))
    }

    fn query_as<O>(
        &self,
        sql: impl AdaptSqlText,
    ) -> sqlx::query::QueryAs<'static, sqlx::Any, O, sqlx::any::AnyArguments>
    where
        O: for<'r> sqlx::FromRow<'r, sqlx::any::AnyRow>,
    {
        #[cfg(test)]
        self.query_count.fetch_add(1, AtomicOrdering::Relaxed);
        sqlx::query_as(adapt_sql_for_backend(self.backend, sql))
    }

    fn query_scalar<O>(
        &self,
        sql: impl AdaptSqlText,
    ) -> sqlx::query::QueryScalar<'static, sqlx::Any, O, sqlx::any::AnyArguments>
    where
        (O,): for<'r> sqlx::FromRow<'r, sqlx::any::AnyRow>,
    {
        #[cfg(test)]
        self.query_count.fetch_add(1, AtomicOrdering::Relaxed);
        sqlx::query_scalar(adapt_sql_for_backend(self.backend, sql))
    }
}

fn sqlite_migrator_with_legacy_timestamp_checksum() -> Option<Migrator> {
    let mut migrations = SQLITE_MIGRATOR.iter().cloned().collect::<Vec<_>>();
    let timestamp_migration = migrations
        .iter_mut()
        .find(|migration| migration.version == 126)?;
    timestamp_migration.checksum = Cow::Borrowed(LEGACY_SQLITE_MIGRATION_0126_CHECKSUM);
    Some(Migrator::with_migrations(migrations))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DashboardStats {
    pub(crate) movie_count: i64,
    pub(crate) series_count: i64,
    pub(crate) user_count: i64,
}

pub(crate) struct CachedDashboardStats {
    pub(crate) computed_at: i64,
    pub(crate) stats: DashboardStats,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct StoredCatalogItemCounts {
    pub(crate) movie_count: i64,
    pub(crate) series_count: i64,
    pub(crate) episode_count: i64,
    pub(crate) box_set_count: i64,
    pub(crate) item_count: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredUser {
    pub(crate) id: String,
    pub(crate) username_normalized: String,
    pub(crate) display_name: String,
    pub(crate) password_hash: String,
    pub(crate) has_password: bool,
    pub(crate) is_disabled: bool,
    pub(crate) is_admin: bool,
    pub(crate) can_manage_server: bool,
    pub(crate) can_remote_access: bool,
    pub(crate) can_download: bool,
    pub(crate) email: Option<String>,
    pub(crate) email_verified_at: Option<i64>,
    pub(crate) deleted_at: Option<i64>,
    pub(crate) deletion_purge_at: Option<i64>,
    pub(crate) last_login_at: Option<i64>,
    pub(crate) last_activity_at: Option<i64>,
}

pub(crate) struct NewStoredUser<'a> {
    pub(crate) id: &'a str,
    pub(crate) username_normalized: &'a str,
    pub(crate) display_name: &'a str,
    pub(crate) password_hash: &'a str,
    pub(crate) is_admin: bool,
    pub(crate) has_password: bool,
    pub(crate) email: Option<&'a str>,
}

// LUX-654 consumes this versioned contract in the account settings API.
#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UserAccountPreferences {
    pub(crate) theme: String,
    pub(crate) accent_color: String,
    pub(crate) show_media_libraries: bool,
    pub(crate) show_continue_watching: bool,
    pub(crate) audio_language: String,
    pub(crate) subtitle_language: String,
    pub(crate) auto_play_next_episode: bool,
    pub(crate) library_order: Vec<String>,
    pub(crate) played_percent: i64,
    pub(crate) version: i64,
    pub(crate) updated_at: i64,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct NewUserAccountPreferences<'a> {
    pub(crate) theme: &'a str,
    pub(crate) accent_color: &'a str,
    pub(crate) show_media_libraries: bool,
    pub(crate) show_continue_watching: bool,
    pub(crate) audio_language: &'a str,
    pub(crate) subtitle_language: &'a str,
    pub(crate) auto_play_next_episode: bool,
    pub(crate) library_order: Option<&'a [String]>,
    pub(crate) played_percent: Option<i64>,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UserAccountPreferencesCasResult {
    Updated(UserAccountPreferences),
    Conflict,
    UserUnavailable,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UserContentRestrictions {
    pub(crate) rating_system: String,
    pub(crate) maximum_rating: Option<String>,
    pub(crate) allow_unrated: bool,
    pub(crate) allowed_tags: Vec<String>,
    pub(crate) blocked_tags: Vec<String>,
    pub(crate) version: i64,
    pub(crate) updated_at: i64,
}

/// Everything the access layer (and Emby policy DTOs) needs about one user's restrictions.
pub(crate) struct UserAccessSnapshot {
    pub(crate) library_access_mode: String,
    pub(crate) library_ids: Vec<String>,
    pub(crate) restrictions: Option<UserContentRestrictions>,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct NewUserContentRestrictions<'a> {
    pub(crate) rating_system: &'a str,
    pub(crate) maximum_rating: Option<&'a str>,
    pub(crate) allow_unrated: bool,
    pub(crate) allowed_tags: &'a [String],
    pub(crate) blocked_tags: &'a [String],
}

#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum UserContentRestrictionsCasResult {
    Updated(UserContentRestrictions),
    Conflict,
    UserUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdminUserStatusFilter {
    All,
    Active,
    Disabled,
    Deleted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdminUserSortField {
    Created,
    LastLogin,
    LastActivity,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredAdminUserListItem {
    pub(crate) id: String,
    pub(crate) username_normalized: String,
    pub(crate) display_name: String,
    pub(crate) has_password: bool,
    pub(crate) is_disabled: bool,
    pub(crate) is_admin: bool,
    pub(crate) can_manage_server: bool,
    pub(crate) can_remote_access: bool,
    pub(crate) can_download: bool,
    pub(crate) email: Option<String>,
    pub(crate) email_verified_at: Option<i64>,
    pub(crate) deleted_at: Option<i64>,
    pub(crate) deletion_purge_at: Option<i64>,
    pub(crate) last_login_at: Option<i64>,
    pub(crate) last_activity_at: Option<i64>,
    pub(crate) created_at: i64,
    pub(crate) current_playback_count: i64,
}

// LUX-648 exposes this stored source in administrator user projections.
#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredUserPermissionPresetSource {
    pub(crate) preset_id: String,
    pub(crate) preset_name: String,
    pub(crate) preset_version: i64,
    pub(crate) applied_at: i64,
}

#[derive(Debug)]
pub(crate) struct StoredAccessTokenDevice {
    pub(crate) device_id: String,
    pub(crate) client_name: String,
    pub(crate) device_name: String,
    pub(crate) client_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredEmbyLoginSessionSummary {
    pub(crate) id: String,
    pub(crate) device_id: String,
    pub(crate) device_name: String,
    pub(crate) device_type: Option<String>,
    pub(crate) client_name: String,
    pub(crate) client_version: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) last_seen_at: Option<i64>,
}

pub(crate) fn stored_user(row: sqlx::any::AnyRow) -> StoredUser {
    StoredUser {
        id: row.get("id"),
        username_normalized: row.get("username_normalized"),
        display_name: row.get("display_name"),
        password_hash: row.get("password_hash"),
        has_password: row.get::<i64, _>("has_password") != 0,
        is_disabled: row.get::<i64, _>("is_disabled") != 0,
        is_admin: row.get::<i64, _>("is_admin") != 0,
        can_manage_server: row.get::<i64, _>("can_manage_server") != 0,
        can_remote_access: row.get::<i64, _>("can_remote_access") != 0,
        can_download: row.get::<i64, _>("can_download") != 0,
        email: row.get("email"),
        email_verified_at: row.get("email_verified_at"),
        deleted_at: row.get("deleted_at"),
        deletion_purge_at: row.get("deletion_purge_at"),
        last_login_at: row.get("last_login_at"),
        last_activity_at: row.get("last_activity_at"),
    }
}

pub(crate) fn stored_admin_user_list_item(row: sqlx::any::AnyRow) -> StoredAdminUserListItem {
    StoredAdminUserListItem {
        id: row.get("id"),
        username_normalized: row.get("username_normalized"),
        display_name: row.get("display_name"),
        has_password: row.get::<i64, _>("has_password") != 0,
        is_disabled: row.get::<i64, _>("is_disabled") != 0,
        is_admin: row.get::<i64, _>("is_admin") != 0,
        can_manage_server: row.get::<i64, _>("can_manage_server") != 0,
        can_remote_access: row.get::<i64, _>("can_remote_access") != 0,
        can_download: row.get::<i64, _>("can_download") != 0,
        email: row.get("email"),
        email_verified_at: row.get("email_verified_at"),
        deleted_at: row.get("deleted_at"),
        deletion_purge_at: row.get("deletion_purge_at"),
        last_login_at: row.get("last_login_at"),
        last_activity_at: row.get("last_activity_at"),
        created_at: row.get("created_at"),
        current_playback_count: row.get("current_playback_count"),
    }
}

// LUX-648 uses this mapping when building administrator user projections.
#[allow(dead_code)]
pub(crate) fn stored_user_permission_preset_source(
    row: sqlx::any::AnyRow,
) -> StoredUserPermissionPresetSource {
    StoredUserPermissionPresetSource {
        preset_id: row.get("preset_id"),
        preset_name: row.get("preset_name"),
        preset_version: row.get("preset_version"),
        applied_at: row.get("applied_at"),
    }
}

pub(crate) struct UpdateUser<'a> {
    pub(crate) display_name: Option<&'a str>,
    pub(crate) password_hash: Option<&'a str>,
    pub(crate) has_password: Option<bool>,
    pub(crate) is_disabled: Option<bool>,
    pub(crate) is_admin: Option<bool>,
    pub(crate) can_manage_server: Option<bool>,
    pub(crate) can_remote_access: Option<bool>,
    pub(crate) can_download: Option<bool>,
}

pub(crate) struct NewAuditEvent<'a> {
    pub(crate) actor_user_id: Option<&'a str>,
    pub(crate) event_type: &'a str,
    pub(crate) target_type: Option<&'a str>,
    pub(crate) target_id: Option<&'a str>,
    pub(crate) metadata_json: &'a str,
}

// LUX-721 wires this persistence contract into the activity event recorder.
#[allow(dead_code)]
pub(crate) struct NewUserActivityEvent<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) actor_user_id: Option<&'a str>,
    pub(crate) event_type: &'a str,
    pub(crate) target_type: Option<&'a str>,
    pub(crate) target_id: Option<&'a str>,
    pub(crate) metadata_json: &'a str,
}

// LUX-742 and LUX-743 connect this device observation contract to successful authentication.
#[allow(dead_code)]
pub(crate) struct NewUserDeviceObservation<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) device_id: &'a str,
    pub(crate) device_name: Option<&'a str>,
    pub(crate) device_type: Option<&'a str>,
    pub(crate) client_name: &'a str,
    pub(crate) client_version: Option<&'a str>,
    pub(crate) last_remote_ip: Option<&'a str>,
    pub(crate) seen_at: i64,
}

// LUX-745 connects this bounded device page to the administrator user API.
#[allow(dead_code)]
pub(crate) struct UserDevicePageQuery<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct StoredUserDevice {
    pub(crate) device_id: String,
    pub(crate) has_active_login: bool,
    pub(crate) device_name: Option<String>,
    pub(crate) device_type: Option<String>,
    pub(crate) client_name: String,
    pub(crate) client_version: Option<String>,
    pub(crate) first_seen_at: i64,
    pub(crate) last_seen_at: i64,
    pub(crate) last_remote_ip: Option<String>,
}

// LUX-755 exposes scope-safe persistence for device, client, and IP rules.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UserAccessBlacklistRuleScope<'a> {
    Global,
    User(&'a str),
}

#[allow(dead_code)]
pub(crate) struct UserAccessBlacklistRulePageQuery<'a> {
    pub(crate) scope: UserAccessBlacklistRuleScope<'a>,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
}

#[allow(dead_code)]
pub(crate) struct UserAccessBlacklistRuleMatchQuery<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) device_id: Option<&'a str>,
    pub(crate) client_name: Option<&'a str>,
}

#[allow(dead_code)]
pub(crate) struct UserAccessBlacklistIpRuleQuery<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) limit: i64,
}

#[allow(dead_code)]
pub(crate) struct NewUserAccessBlacklistRule<'a> {
    pub(crate) id: &'a str,
    pub(crate) user_id: Option<&'a str>,
    pub(crate) rule_type: &'a str,
    pub(crate) match_value: &'a str,
    pub(crate) is_enabled: bool,
    pub(crate) created_by: Option<&'a str>,
    pub(crate) created_at: i64,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct StoredUserAccessBlacklistRule {
    pub(crate) id: String,
    pub(crate) user_id: Option<String>,
    pub(crate) rule_type: String,
    pub(crate) match_value: String,
    pub(crate) is_enabled: bool,
    pub(crate) created_by: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

// LUX-723 consumes this bounded query contract in the administrator activity API.
#[allow(dead_code)]
pub(crate) struct UserActivityEventQuery<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) event_type: Option<&'a str>,
    pub(crate) from_at: Option<i64>,
    pub(crate) to_at: Option<i64>,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
}

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct StoredUserActivityEvent {
    pub(crate) id: String,
    pub(crate) user_id: String,
    pub(crate) actor_user_id: Option<String>,
    pub(crate) event_type: String,
    pub(crate) target_type: Option<String>,
    pub(crate) target_id: Option<String>,
    pub(crate) metadata_json: String,
    pub(crate) occurred_at: i64,
}

#[derive(Debug)]
pub(crate) struct StoredActivityEvent {
    pub(crate) id: String,
    pub(crate) actor_user_id: Option<String>,
    pub(crate) actor_username: Option<String>,
    pub(crate) event_type: String,
    pub(crate) target_type: Option<String>,
    pub(crate) target_id: Option<String>,
    pub(crate) target_title: Option<String>,
    pub(crate) metadata_json: String,
    pub(crate) created_at: i64,
}

#[derive(Debug)]
pub(crate) struct StoredLibrary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) is_enabled: bool,
    pub(crate) realtime_watch_enabled: bool,
    pub(crate) realtime_metadata_auto_match_enabled: bool,
    pub(crate) incremental_schedule: Option<String>,
    pub(crate) reconciliation_schedule: Option<String>,
    pub(crate) metadata_schedule: Option<String>,
    pub(crate) scan_concurrency: i64,
    pub(crate) probe_concurrency: i64,
    pub(crate) last_scan_at: Option<i64>,
    pub(crate) scraper_id: Option<String>,
    pub(crate) scrapers: Vec<StoredLibraryScraper>,
    pub(crate) chapter_source_id: Option<String>,
    pub(crate) cover_image_path: Option<String>,
    pub(crate) cover_image_content_type: Option<String>,
    pub(crate) cover_image_size: Option<i64>,
    pub(crate) cover_image_tag: Option<String>,
    pub(crate) media_strategy_json: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredLibraryIdentity {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) root_paths: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredLibraryDeletionJob {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) library_name: String,
    pub(crate) status: String,
    pub(crate) stage: String,
    pub(crate) processed_count: i64,
    pub(crate) error_code: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct StoredLibraryScraper {
    pub(crate) scraper_id: String,
    pub(crate) position: i64,
    pub(crate) role: String,
}

#[derive(Debug)]
pub(crate) struct StoredScheduledTaskConfig {
    pub(crate) owner_type: String,
    pub(crate) owner_id: String,
    pub(crate) task_type: String,
    pub(crate) plan_id: Option<String>,
    pub(crate) task_name: String,
    pub(crate) task_description: String,
    pub(crate) source_type: String,
    pub(crate) plugin_id: Option<String>,
    pub(crate) cron_or_interval: Option<String>,
    pub(crate) is_enabled: bool,
    pub(crate) resource_limit_json: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) library_name: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredScheduledTaskPlan {
    pub(crate) id: String,
    pub(crate) task_type: String,
    pub(crate) plan_name: String,
    pub(crate) task_name: String,
    pub(crate) task_description: String,
    pub(crate) source_type: String,
    pub(crate) plugin_id: Option<String>,
    pub(crate) cron_or_interval: Option<String>,
    pub(crate) is_enabled: bool,
    pub(crate) resource_limit_json: String,
    pub(crate) scope_type: String,
    pub(crate) is_default: bool,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) libraries: Vec<StoredScheduledTaskPlanLibrary>,
}

#[derive(Debug)]
pub(crate) struct StoredScheduledTaskPlanLibrary {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Debug)]
pub(crate) struct StoredNotificationDestination {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) enabled: bool,
    pub(crate) allow_private_network: bool,
    pub(crate) event_types_json: String,
    pub(crate) payload_format: String,
    pub(crate) provider_plugin_id: String,
    pub(crate) provider_config_json: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

pub(crate) struct NewNotificationDestination<'a> {
    pub(crate) id: &'a str,
    pub(crate) name: &'a str,
    pub(crate) url: &'a str,
    pub(crate) enabled: bool,
    pub(crate) allow_private_network: bool,
    pub(crate) event_types_json: &'a str,
    pub(crate) payload_format: &'a str,
    pub(crate) provider_plugin_id: &'a str,
    pub(crate) provider_config_json: &'a str,
}

pub(crate) struct UpdateNotificationDestination<'a> {
    pub(crate) name: Option<&'a str>,
    pub(crate) url: Option<&'a str>,
    pub(crate) enabled: Option<bool>,
    pub(crate) allow_private_network: Option<bool>,
    pub(crate) event_types_json: Option<&'a str>,
    pub(crate) payload_format: Option<&'a str>,
    pub(crate) provider_plugin_id: Option<&'a str>,
    pub(crate) provider_config_json: Option<&'a str>,
}

#[derive(Debug)]
pub(crate) struct StoredNotificationDelivery {
    pub(crate) id: String,
    pub(crate) event_id: String,
    pub(crate) destination_id: String,
    pub(crate) status: String,
    pub(crate) attempt_count: i64,
    pub(crate) next_attempt_at: i64,
    pub(crate) last_http_status: Option<i64>,
    pub(crate) last_error: Option<String>,
    pub(crate) delivered_at: Option<i64>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) event_type: String,
    pub(crate) payload_json: String,
    pub(crate) destination_name: String,
    pub(crate) destination_url: String,
    pub(crate) allow_private_network: bool,
    pub(crate) provider_plugin_id: String,
    pub(crate) provider_config_json: String,
}

pub(crate) struct NewNotificationEvent<'a> {
    pub(crate) id: &'a str,
    pub(crate) event_type: &'a str,
    pub(crate) schema_version: i64,
    pub(crate) occurred_at: i64,
    pub(crate) dedupe_key: &'a str,
    pub(crate) payload_json: &'a str,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredLibraryRoot {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) canonical_path: String,
    pub(crate) display_path: String,
    pub(crate) is_available: bool,
    pub(crate) is_writable: bool,
    pub(crate) last_checked_at: i64,
    pub(crate) unavailable_since: Option<i64>,
    pub(crate) scan_cursor: Option<String>,
}

fn stored_scheduled_task(row: sqlx::any::AnyRow) -> StoredScheduledTaskConfig {
    StoredScheduledTaskConfig {
        owner_type: row.get("owner_type"),
        owner_id: row.get("owner_id"),
        task_type: row.get("task_type"),
        plan_id: row.get("plan_id"),
        task_name: row.get("task_name"),
        task_description: row.get("task_description"),
        source_type: row.get("source_type"),
        plugin_id: row.get("plugin_id"),
        cron_or_interval: row.get("cron_or_interval"),
        is_enabled: row.get::<i64, _>("is_enabled") != 0,
        resource_limit_json: row.get("resource_limit_json"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        library_name: row.get("library_name"),
    }
}

fn stored_scheduled_task_plan(row: sqlx::any::AnyRow) -> StoredScheduledTaskPlan {
    StoredScheduledTaskPlan {
        id: row.get("id"),
        task_type: row.get("task_type"),
        plan_name: row.get("plan_name"),
        task_name: row.get("task_name"),
        task_description: row.get("task_description"),
        source_type: row.get("source_type"),
        plugin_id: row.get("plugin_id"),
        cron_or_interval: row.get("cron_or_interval"),
        is_enabled: row.get::<i64, _>("is_enabled") != 0,
        resource_limit_json: row.get("resource_limit_json"),
        scope_type: row.get("scope_type"),
        is_default: row.get::<i64, _>("is_default") != 0,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        libraries: Vec::new(),
    }
}

fn stored_notification_destination(row: sqlx::any::AnyRow) -> StoredNotificationDestination {
    StoredNotificationDestination {
        id: row.get("id"),
        name: row.get("name"),
        url: row.get("url"),
        enabled: row.get::<i64, _>("enabled") != 0,
        allow_private_network: row.get::<i64, _>("allow_private_network") != 0,
        event_types_json: row.get("event_types_json"),
        payload_format: row.get("payload_format"),
        provider_plugin_id: row.get("provider_plugin_id"),
        provider_config_json: row.get("provider_config_json"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn stored_notification_delivery(row: sqlx::any::AnyRow) -> StoredNotificationDelivery {
    StoredNotificationDelivery {
        id: row.get("id"),
        event_id: row.get("event_id"),
        destination_id: row.get("destination_id"),
        status: row.get("status"),
        attempt_count: row.get("attempt_count"),
        next_attempt_at: row.get("next_attempt_at"),
        last_http_status: row.get("last_http_status"),
        last_error: row.get("last_error"),
        delivered_at: row.get("delivered_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        event_type: row.get("event_type"),
        payload_json: row.get("payload_json"),
        destination_name: row.get("name"),
        destination_url: row.get("url"),
        allow_private_network: row.get::<i64, _>("allow_private_network") != 0,
        provider_plugin_id: row.get("provider_plugin_id"),
        provider_config_json: row.get("provider_config_json"),
    }
}

#[derive(Debug)]
pub(crate) struct StoredScanJob {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) job_type: String,
    pub(crate) status: String,
    pub(crate) generation: String,
    pub(crate) cursor: Option<String>,
    pub(crate) processed_count: i64,
    pub(crate) total_count: i64,
    pub(crate) processed_media_count: Option<i64>,
    pub(crate) cancel_requested: bool,
    pub(crate) error: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
    pub(crate) discovery_completed: bool,
    pub(crate) auto_metadata_match: bool,
    pub(crate) current_item: Option<String>,
    pub(crate) scan_phase: String,
}

#[derive(Debug, Default)]
pub(crate) struct StoredScanJobRetryState {
    pub(crate) has_postprocessing_issues: bool,
    pub(crate) has_unfinished_targets: bool,
    pub(crate) targets_need_materialization: bool,
    pub(crate) has_legacy_work: bool,
    pub(crate) has_active_job: bool,
}

#[allow(dead_code)] // Storage tests exercise the single-batch enqueue contract.
#[derive(Clone, Copy)]
pub(crate) struct NewScanLocalMetadataBatch<'a> {
    pub(crate) id: &'a str,
    pub(crate) job_id: &'a str,
    pub(crate) library_root_id: &'a str,
    pub(crate) batch_sequence: i64,
    pub(crate) source_ids: &'a [String],
}

#[allow(dead_code)] // The active worker reads a subset; tests inspect the complete stored row.
#[derive(Debug)]
pub(crate) struct StoredScanLocalMetadataBatch {
    pub(crate) id: String,
    pub(crate) job_id: String,
    pub(crate) library_root_id: String,
    pub(crate) batch_sequence: i64,
    pub(crate) source_refs_json: String,
    pub(crate) non_retryable_item_ids_json: String,
    pub(crate) source_count: i64,
    pub(crate) images_completed_at: Option<i64>,
    pub(crate) status: String,
    pub(crate) attempts: i64,
    pub(crate) next_attempt_at: Option<i64>,
    pub(crate) error: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

#[derive(Debug)]
pub(crate) struct StoredScanLocalMetadataSource {
    pub(crate) source_id: String,
    pub(crate) item_id: String,
    pub(crate) item_type: String,
    pub(crate) probe_status: String,
    pub(crate) series_id: Option<String>,
    pub(crate) season_id: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NewScanManifestRoot<'a> {
    pub(crate) library_root_id: &'a str,
}

#[derive(Debug)]
pub(crate) struct NewScanManifest<'a> {
    pub(crate) id: &'a str,
    pub(crate) job_id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) roots: &'a [NewScanManifestRoot<'a>],
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // LUX-267 supplies computed reconciliation deltas through this boundary.
pub(crate) struct NewScanManifestDelta<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_root_id: &'a str,
    pub(crate) relative_path: &'a str,
    pub(crate) observation_sequence: Option<i64>,
    pub(crate) delta_kind: &'a str,
    pub(crate) base_filesystem_entry_id: Option<&'a str>,
    pub(crate) base_fingerprint: Option<&'a [u8]>,
}

#[derive(Clone, Debug)]
pub(crate) struct NewScanManifestSidecarEntry {
    pub(crate) filesystem_entry_id: String,
    pub(crate) relative_path: String,
}

#[derive(Clone)]
pub(crate) enum NewScanManifestIndexedFile {
    Movie(NewMovieFile),
    Episode(NewEpisodeFile),
    Unresolved(NewScanManifestUnresolvedFile),
    Sidecar(NewScanManifestSidecarEntry),
}

#[derive(Clone)]
pub(crate) struct NewScanManifestPositiveIndex {
    pub(crate) relative_path: String,
    pub(crate) delta_kind: String,
    pub(crate) base_filesystem_entry_id: Option<String>,
    pub(crate) base_fingerprint: Option<Vec<u8>>,
    pub(crate) file: NewScanManifestIndexedFile,
}

/// An existing filesystem entry observed unchanged during a format-3 directory read.
///
/// The scan generation is advanced directly on the canonical filesystem row instead of
/// inserting a duplicate row into the manifest presence ledger. The entry id and observed
/// fingerprint form the CAS anchor: a missing, replaced, or concurrently changed row is never
/// marked as seen by this optimization.
#[derive(Clone, Debug)]
pub(crate) struct NewScanManifestSeenFilesystemEntry {
    pub(crate) filesystem_entry_id: String,
    pub(crate) relative_path: String,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: Vec<u8>,
}

pub(crate) struct NewScanManifestFilesystemEntry<'a> {
    pub(crate) id: &'a str,
    pub(crate) relative_path: &'a str,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: &'a [u8],
    pub(crate) last_seen_change_kind: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub(crate) struct NewScanManifestUnresolvedFile {
    pub(crate) item_id: String,
    pub(crate) filesystem_entry_id: String,
    pub(crate) source_id: String,
    pub(crate) relative_path: String,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: Vec<u8>,
    pub(crate) title: String,
    pub(crate) identity_key: String,
    pub(crate) source_kind: String,
    pub(crate) container: String,
    pub(crate) external_url: Option<String>,
    pub(crate) strm_target_kind: Option<String>,
    pub(crate) home_video: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ManifestExistingFileUpdate<'a> {
    pub(crate) filesystem_entry_id: &'a str,
    pub(crate) library_root_id: &'a str,
    pub(crate) relative_path: &'a str,
    pub(crate) base_fingerprint: Option<&'a [u8]>,
    pub(crate) expected_missing: bool,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: &'a [u8],
    pub(crate) generation: &'a str,
    pub(crate) last_seen_change_kind: Option<&'a str>,
    pub(crate) source_kind: &'a str,
    pub(crate) edition_name: Option<&'a str>,
    pub(crate) quality_label: Option<&'a str>,
    pub(crate) container: &'a str,
    pub(crate) external_url: Option<&'a str>,
    pub(crate) strm_target_kind: Option<&'a str>,
}

pub(crate) struct ManifestDeltaBatchCommit<'a> {
    pub(crate) job_id: &'a str,
    pub(crate) manifest_id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) library_root_id: &'a str,
    pub(crate) generation: &'a str,
    pub(crate) deltas: &'a [StoredScanManifestDelta],
    pub(crate) unstable_delta_ids: &'a [String],
    pub(crate) movie_files: &'a [NewMovieFile],
    pub(crate) episode_files: &'a [NewEpisodeFile],
    pub(crate) unresolved_files: &'a [NewScanManifestUnresolvedFile],
    pub(crate) sidecar_entries: &'a [NewScanManifestSidecarEntry],
    pub(crate) removed_media_paths: &'a [String],
    pub(crate) removed_sidecar_paths: &'a [String],
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct ManifestDeltaBatchCommitResult {
    pub(crate) applied_count: usize,
    pub(crate) conflict_count: usize,
    pub(crate) unstable_count: usize,
    pub(crate) created_items: usize,
    pub(crate) metadata_targets_changed: bool,
    pub(crate) removed_count: usize,
}

#[derive(Debug)]
#[allow(dead_code)] // LUX-267 consumes the persisted diff and apply counters.
pub(crate) struct StoredScanManifest {
    pub(crate) id: String,
    pub(crate) job_id: String,
    pub(crate) library_id: String,
    pub(crate) state: String,
    pub(crate) workflow_version: i64,
    pub(crate) discovery_format_version: i64,
    pub(crate) discovery_mode: String,
    pub(crate) root_count: i64,
    pub(crate) discovered_directory_count: i64,
    pub(crate) completed_directory_count: i64,
    pub(crate) observed_file_count: i64,
    pub(crate) unchanged_count: i64,
    pub(crate) add_count: i64,
    pub(crate) change_count: i64,
    pub(crate) remove_count: i64,
    pub(crate) reappeared_count: i64,
    pub(crate) applied_delta_count: i64,
    pub(crate) postprocessing_targets_ready: bool,
}

pub(crate) fn is_lite_manifest_discovery(
    workflow_version: i64,
    discovery_format_version: i64,
    discovery_mode: &str,
) -> bool {
    matches!(workflow_version, 2 | 3) && discovery_format_version == 3 && discovery_mode == "LITE"
}

#[derive(Debug)]
pub(crate) struct StoredScanManifestPostprocessingRoot {
    pub(crate) library_root_id: String,
    pub(crate) canonical_path: String,
    pub(crate) expected_device: Option<i64>,
    pub(crate) expected_inode: Option<i64>,
    pub(crate) target_stage: String,
    pub(crate) target_cursor: Option<String>,
    pub(crate) has_stage_rows: bool,
    pub(crate) has_positive_rows: bool,
}

#[derive(Debug, Default)]
pub(crate) struct ManifestPostprocessingTargetBatchResult {
    pub(crate) targets_changed: bool,
    pub(crate) targets_ready: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ManifestPostprocessingTargetPage<'a> {
    pub(crate) job_id: &'a str,
    pub(crate) manifest_id: &'a str,
    pub(crate) library_root_id: &'a str,
    pub(crate) generation: &'a str,
    pub(crate) target_stage: &'a str,
    pub(crate) target_cursor: Option<&'a str>,
    pub(crate) page_size: usize,
}

#[derive(Debug)]
pub(crate) struct StoredScanManifestDiffCandidate {
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
    pub(crate) observation_sequence: i64,
    pub(crate) delta_kind: String,
    pub(crate) base_filesystem_entry_id: Option<String>,
    pub(crate) base_fingerprint: Option<Vec<u8>>,
    pub(crate) base_entry_kind: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredScanManifestFilesystemBaseline {
    pub(crate) id: String,
    pub(crate) entry_kind: String,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: Option<Vec<u8>>,
    pub(crate) is_missing: bool,
}

#[derive(Debug)]
pub(crate) struct StoredScanManifestRemovalCandidate {
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
    pub(crate) base_filesystem_entry_id: String,
    pub(crate) base_fingerprint: Option<Vec<u8>>,
}

pub(crate) struct ScanManifestRemovalCandidateQuery<'a> {
    pub(crate) manifest_id: &'a str,
    pub(crate) library_root_id: &'a str,
    pub(crate) discovery_format_version: i64,
    pub(crate) uncovered_only: bool,
    pub(crate) directories: Option<&'a [StoredScanManifestDirectory]>,
    pub(crate) after_relative_path: Option<&'a str>,
    pub(crate) limit: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredScanManifestDelta {
    pub(crate) id: String,
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
    pub(crate) observation_sequence: Option<i64>,
    pub(crate) delta_kind: String,
    pub(crate) base_filesystem_entry_id: Option<String>,
    pub(crate) base_fingerprint: Option<Vec<u8>>,
    pub(crate) entry_kind: Option<String>,
    pub(crate) size: Option<i64>,
    pub(crate) modified_at: Option<i64>,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub(crate) struct NewScanManifestEntry {
    pub(crate) relative_path: String,
    pub(crate) entry_kind: String,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: Vec<u8>,
    pub(crate) fingerprint_pending: bool,
    /// The observation was produced by a symbolic link rather than by the file
    /// itself; `resolved_path` holds the canonical target it resolved to.
    pub(crate) is_symlink: bool,
    pub(crate) resolved_path: Option<String>,
}

pub(crate) struct NewScanManifestDiscoveryChunk<'a> {
    pub(crate) manifest_id: &'a str,
    pub(crate) job_id: &'a str,
    pub(crate) library_root_id: &'a str,
    pub(crate) child_directories: &'a [String],
    pub(crate) entries: &'a [NewScanManifestEntry],
    pub(crate) positive_indexes: &'a [NewScanManifestPositiveIndex],
    pub(crate) unchanged_paths: &'a [String],
    pub(crate) seen_filesystem_entries: &'a [NewScanManifestSeenFilesystemEntry],
    pub(crate) completed_directory: Option<&'a str>,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct ManifestDiscoveryCommitResult {
    pub(crate) observed_file_count: i64,
    pub(crate) created_items: usize,
    pub(crate) metadata_targets_changed: bool,
    pub(crate) local_metadata_batches_changed: bool,
}

#[derive(Debug)]
pub(crate) struct StoredScanManifestDirectory {
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
}

#[derive(Debug)]
pub(crate) struct StoredScanJobPath {
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
    pub(crate) change_kind: String,
    // The file-change worker consumes this only in LUX-539; normal scan paths have no revision.
    #[allow(dead_code)]
    pub(crate) file_change_revision: Option<i64>,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct StoredScanJobCounts {
    pub(crate) running: i64,
    pub(crate) failed: i64,
}

#[derive(Debug)]
pub(crate) struct ChapterDetectionSourceStateUpdate {
    pub(crate) input_fingerprint: Vec<u8>,
    pub(crate) status: String,
    pub(crate) last_checked_at: i64,
    pub(crate) last_success_at: Option<i64>,
    pub(crate) next_retry_at: Option<i64>,
    pub(crate) error: Option<String>,
    pub(crate) intro_fingerprint: Option<Vec<u8>>,
    pub(crate) credits_fingerprint: Option<Vec<u8>>,
}

#[derive(Debug)]
pub(crate) struct ChapterDetectionOutcomeUpdate {
    pub(crate) source_id: String,
    pub(crate) status: String,
    pub(crate) error: Option<String>,
    pub(crate) source_state: Option<ChapterDetectionSourceStateUpdate>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredReconciliationScanEntry {
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
}

/// Prepared data for one bounded reconciliation commit.
///
/// Filesystem work and parsing happen before this value is built. The storage
/// layer is responsible for making the media/index, target, seen-entry and
/// reconciliation checkpoint changes one transaction. `entries` must contain
/// FILE work items for `library_root_id`; a later scanner integration can call
/// this once per root and bounded batch.
pub(crate) struct ReconciliationBatchCommit<'a> {
    pub(crate) job_id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) library_root_id: &'a str,
    pub(crate) generation: &'a str,
    pub(crate) entries: &'a [StoredReconciliationScanEntry],
    pub(crate) movie_files: &'a [NewMovieFile],
    pub(crate) episode_files: &'a [NewEpisodeFile],
    pub(crate) seen_entry_ids: &'a [String],
    pub(crate) missing_paths: &'a [String],
    pub(crate) new_paths: &'a [String],
    pub(crate) changed_paths: &'a [String],
    pub(crate) sidecar_paths: &'a [String],
    pub(crate) file_change_events: &'a [(String, String)],
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ReconciliationBatchCommitResult {
    pub(crate) confirmed_entries: usize,
    pub(crate) created_items: usize,
    pub(crate) metadata_targets_changed: bool,
}

#[derive(Debug)]
pub(crate) struct StoredStrmProbeJob {
    pub(crate) id: String,
    pub(crate) operation_id: String,
    pub(crate) library_id: String,
    pub(crate) status: String,
    pub(crate) concurrency: i64,
    pub(crate) include_ready: bool,
    pub(crate) write_sidecars: bool,
    pub(crate) media_info_enabled: bool,
    pub(crate) thumbnail_enabled: bool,
    pub(crate) thumbnail_position_percent: i64,
    pub(crate) target_scan_job_id: Option<String>,
    pub(crate) target_source_id: Option<String>,
    pub(crate) cursor: Option<String>,
    pub(crate) processed_count: i64,
    pub(crate) total_count: i64,
    pub(crate) cancel_requested: bool,
    pub(crate) error: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
}

pub(crate) struct NewStrmProbeJob<'a> {
    pub(crate) id: &'a str,
    pub(crate) operation_id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) concurrency: i64,
    pub(crate) include_ready: bool,
    pub(crate) write_sidecars: bool,
    pub(crate) media_info_enabled: bool,
    pub(crate) thumbnail_enabled: bool,
    pub(crate) thumbnail_position_percent: i64,
    pub(crate) target_scan_job_id: Option<&'a str>,
    pub(crate) target_source_id: Option<&'a str>,
    pub(crate) total_count: i64,
}

fn stored_scan_job(row: sqlx::any::AnyRow) -> StoredScanJob {
    StoredScanJob {
        id: row.get("id"),
        library_id: row.get("library_id"),
        job_type: row.get("job_type"),
        status: row.get("status"),
        generation: row.get("generation"),
        cursor: row.get("cursor"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        processed_media_count: row.get("processed_media_count"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
        error: row.get("error"),
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
        discovery_completed: row.get::<i64, _>("discovery_completed") != 0,
        auto_metadata_match: row.get::<i64, _>("auto_metadata_match") != 0,
        current_item: row.get("current_item"),
        scan_phase: row.get("scan_phase"),
    }
}

fn stored_scan_local_metadata_batch(row: sqlx::any::AnyRow) -> StoredScanLocalMetadataBatch {
    StoredScanLocalMetadataBatch {
        id: row.get("id"),
        job_id: row.get("job_id"),
        library_root_id: row.get("library_root_id"),
        batch_sequence: row.get("batch_sequence"),
        source_refs_json: row.get("source_refs_json"),
        non_retryable_item_ids_json: row.get("non_retryable_item_ids_json"),
        source_count: row.get("source_count"),
        images_completed_at: row.get("images_completed_at"),
        status: row.get("status"),
        attempts: row.get("attempts"),
        next_attempt_at: row.get("next_attempt_at"),
        error: row.get("error"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn stored_scan_job_path(row: sqlx::any::AnyRow) -> StoredScanJobPath {
    StoredScanJobPath {
        library_root_id: row.get("library_root_id"),
        relative_path: row.get("relative_path"),
        change_kind: row.get("change_kind"),
        file_change_revision: None,
    }
}

fn stored_reconciliation_scan_entry(row: sqlx::any::AnyRow) -> StoredReconciliationScanEntry {
    StoredReconciliationScanEntry {
        library_root_id: row.get("library_root_id"),
        relative_path: row.get("relative_path"),
    }
}

fn stored_strm_probe_job(row: sqlx::any::AnyRow) -> StoredStrmProbeJob {
    StoredStrmProbeJob {
        id: row.get("id"),
        operation_id: row.get("operation_id"),
        library_id: row.get("library_id"),
        status: row.get("status"),
        concurrency: row.get("concurrency"),
        include_ready: row.get::<i64, _>("include_ready") != 0,
        write_sidecars: row.get::<i64, _>("write_sidecars") != 0,
        media_info_enabled: row.get::<i64, _>("media_info_enabled") != 0,
        thumbnail_enabled: row.get::<i64, _>("thumbnail_enabled") != 0,
        thumbnail_position_percent: row.get("thumbnail_position_percent"),
        target_scan_job_id: row.get("target_scan_job_id"),
        target_source_id: row.get("target_source_id"),
        cursor: row.get("cursor"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
        error: row.get("error"),
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
    }
}

fn stored_chapter_detection_job(row: sqlx::any::AnyRow) -> StoredChapterDetectionJob {
    StoredChapterDetectionJob {
        id: row.get("id"),
        library_id: row.get("library_id"),
        plugin_id: row.get("plugin_id"),
        status: row.get("status"),
        concurrency: row.get("concurrency"),
        intro_window_seconds: row.get("intro_window_seconds"),
        credits_window_seconds: row.get("credits_window_seconds"),
        match_threshold: row.get("match_threshold"),
        cursor: row.get("cursor"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
        error: row.get("error"),
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
    }
}

fn stored_library_root(row: sqlx::any::AnyRow) -> StoredLibraryRoot {
    StoredLibraryRoot {
        id: row.get("id"),
        library_id: row.get("library_id"),
        canonical_path: row.get("canonical_path"),
        display_path: row.get("display_path"),
        is_available: row.get::<i64, _>("is_available") != 0,
        is_writable: row.get::<i64, _>("is_writable") != 0,
        last_checked_at: row.get("last_checked_at"),
        unavailable_since: row.get("unavailable_since"),
        scan_cursor: row.get("scan_cursor"),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct StoredFilesystemEntry {
    pub(crate) id: String,
    pub(crate) relative_path: String,
    pub(crate) entry_kind: String,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: Option<Vec<u8>>,
    pub(crate) last_seen_generation: String,
    pub(crate) is_missing: bool,
    pub(crate) item_id: Option<String>,
    pub(crate) parent_identity_key: Option<String>,
    pub(crate) item_type: Option<String>,
    pub(crate) item_identity_key: Option<String>,
    pub(crate) series_provider_ids_json: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredScanFingerprintMigrationCandidate {
    pub(crate) entry_id: String,
    pub(crate) library_id: String,
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
    pub(crate) root_path: String,
    pub(crate) root_is_available: bool,
    pub(crate) fingerprint: Vec<u8>,
    pub(crate) attempts: i64,
}

pub(crate) struct ScanFingerprintMigrationCommit<'a> {
    pub(crate) candidate: &'a StoredScanFingerprintMigrationCandidate,
    pub(crate) fingerprint_v2: &'a [u8],
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: i64,
    pub(crate) inode: i64,
    pub(crate) lease_token: &'a str,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredScanFingerprintMigrationCursor {
    pub(crate) root_id: Option<String>,
    pub(crate) relative_path: Option<String>,
    pub(crate) completed_at: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct StoredItemSourceLocator {
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
    pub(crate) fingerprint: Option<Vec<u8>>,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) title: String,
    pub(crate) production_year: Option<i32>,
}

#[derive(Debug)]
pub(crate) struct StoredEpisodeIdentityCandidate {
    pub(crate) episode_id: String,
    pub(crate) filesystem_entry_id: String,
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
}

fn stored_filesystem_entry(row: sqlx::any::AnyRow) -> StoredFilesystemEntry {
    StoredFilesystemEntry {
        id: row.get("id"),
        relative_path: row.get("relative_path"),
        entry_kind: row.get("entry_kind"),
        size: row.get("size"),
        modified_at: row.get("modified_at"),
        device: row.get("device"),
        inode: row.get("inode"),
        fingerprint: row.get("fingerprint"),
        last_seen_generation: row.get("last_seen_generation"),
        is_missing: row.get::<i64, _>("is_missing") != 0,
        item_id: row.get("item_id"),
        parent_identity_key: row.get("parent_identity_key"),
        item_type: row.get("item_type"),
        item_identity_key: row.get("item_identity_key"),
        series_provider_ids_json: row.get("series_provider_ids_json"),
    }
}

#[derive(Debug)]
pub(crate) struct StoredMediaItem {
    pub(crate) id: String,
}

#[derive(Clone, Debug)]
struct PrefetchedMovieItem {
    id: String,
    parent_id: Option<String>,
    provider_ids_json: Option<String>,
    removed_at: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct NewPersonCredit {
    pub(crate) person_id: String,
    pub(crate) lux_person_id: Option<String>,
    pub(crate) person_type: String,
    pub(crate) person_name: String,
    pub(crate) provider: String,
    pub(crate) role: String,
    pub(crate) sort_order: i64,
    pub(crate) biography: Option<String>,
    pub(crate) birthday: Option<String>,
    pub(crate) deathday: Option<String>,
    pub(crate) known_for_department: Option<String>,
    pub(crate) place_of_birth: Option<String>,
    pub(crate) provider_ids: BTreeMap<String, String>,
    pub(crate) genres: Vec<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) production_locations: Vec<String>,
    pub(crate) premiere_date: Option<String>,
    pub(crate) production_year: Option<i64>,
    pub(crate) taglines: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct StoredCanonicalPerson {
    pub(crate) id: String,
}

#[derive(Debug)]
pub(crate) struct StoredCanonicalPersonMatch {
    pub(crate) id: String,
    pub(crate) birthdays: Vec<String>,
}

fn stored_canonical_person(row: sqlx::any::AnyRow) -> StoredCanonicalPerson {
    StoredCanonicalPerson { id: row.get("id") }
}

#[derive(Debug)]
pub(crate) struct StoredPersonMatchCandidate {
    pub(crate) id: String,
    pub(crate) item_id: String,
    pub(crate) provider: String,
    pub(crate) provider_id: String,
    pub(crate) candidate_person_ids_json: String,
    pub(crate) status: String,
    pub(crate) score: Option<f64>,
    pub(crate) evidence_json: String,
    pub(crate) target_person_id: Option<String>,
    pub(crate) previous_person_id: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

pub(crate) struct PersonMatchCandidateRestore<'a> {
    pub(crate) candidate_id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) provider: &'a str,
    pub(crate) provider_id: &'a str,
    pub(crate) candidate_person_ids_json: &'a str,
    pub(crate) status: &'a str,
    pub(crate) score: Option<f64>,
    pub(crate) evidence_json: &'a str,
    pub(crate) target_person_id: Option<&'a str>,
    pub(crate) previous_person_id: Option<&'a str>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

fn stored_person_match_candidate(row: sqlx::any::AnyRow) -> StoredPersonMatchCandidate {
    StoredPersonMatchCandidate {
        id: row.get("id"),
        item_id: row.get("item_id"),
        provider: row.get("provider"),
        provider_id: row.get("provider_id"),
        candidate_person_ids_json: row.get("candidate_person_ids_json"),
        status: row.get("status"),
        score: row.get("score"),
        evidence_json: row.get("evidence_json"),
        target_person_id: row.get("target_person_id"),
        previous_person_id: row.get("previous_person_id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

#[derive(Debug)]
pub(crate) struct StoredPersonIdentityMove {
    pub(crate) previous_person_id: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredPersonCredit {
    pub(crate) item_id: String,
    pub(crate) person_id: String,
    pub(crate) lux_person_id: Option<String>,
    pub(crate) provider: String,
    pub(crate) person_name: String,
    pub(crate) role: String,
    pub(crate) date_created: i64,
    pub(crate) biography: Option<String>,
    pub(crate) birthday: Option<String>,
    pub(crate) deathday: Option<String>,
    pub(crate) known_for_department: Option<String>,
    pub(crate) place_of_birth: Option<String>,
    pub(crate) provider_ids: BTreeMap<String, String>,
    pub(crate) genres: Vec<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) production_locations: Vec<String>,
    pub(crate) premiere_date: Option<String>,
    pub(crate) production_year: Option<i64>,
    pub(crate) taglines: Vec<String>,
}

/// Decodes a person-credit JSON column, reporting corruption instead of silently emptying it.
fn person_credit_json<T: serde::de::DeserializeOwned + Default>(
    value: &str,
    column: &'static str,
) -> T {
    serde_json::from_str(value).unwrap_or_else(|error| {
        tracing::warn!(%error, column, "person credit column holds invalid JSON");
        T::default()
    })
}

fn stored_person_credit(row: sqlx::any::AnyRow) -> StoredPersonCredit {
    let provider_ids_json: String = row.get("provider_ids_json");
    let genres_json: String = row.get("genres_json");
    let tags_json: String = row.get("tags_json");
    let production_locations_json: String = row.get("production_locations_json");
    let taglines_json: String = row.get("taglines_json");
    StoredPersonCredit {
        item_id: row.get("item_id"),
        person_id: row.get("person_id"),
        lux_person_id: row.try_get("lux_person_id").ok(),
        provider: row.get("provider"),
        person_name: row.get("person_name"),
        role: row.get("role"),
        date_created: row.get("date_created"),
        biography: row.get("biography"),
        birthday: row.get("birthday"),
        deathday: row.get("deathday"),
        known_for_department: row.get("known_for_department"),
        place_of_birth: row.get("place_of_birth"),
        provider_ids: person_credit_json(&provider_ids_json, "provider_ids_json"),
        genres: person_credit_json(&genres_json, "genres_json"),
        tags: person_credit_json(&tags_json, "tags_json"),
        production_locations: person_credit_json(
            &production_locations_json,
            "production_locations_json",
        ),
        premiere_date: row.get("premiere_date"),
        production_year: row.get("production_year"),
        taglines: person_credit_json(&taglines_json, "taglines_json"),
    }
}

#[derive(Debug)]
pub(crate) struct StoredPersonIndexRebuildJob {
    pub(crate) library_id: String,
    pub(crate) status: String,
    pub(crate) cursor_id: Option<String>,
    pub(crate) processed_count: i64,
    pub(crate) total_count: i64,
    pub(crate) cancel_requested: bool,
}

fn stored_person_index_rebuild_job(row: sqlx::any::AnyRow) -> StoredPersonIndexRebuildJob {
    StoredPersonIndexRebuildJob {
        library_id: row.get("library_id"),
        status: row.get("status"),
        cursor_id: row.get("cursor_id"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
    }
}

#[derive(Debug)]
pub(crate) struct StoredCollectionRefresh {
    pub(crate) collection_item_id: String,
    pub(crate) member_count: usize,
}

pub(crate) const GLOBAL_COLLECTION_LIBRARY_ID: &str = "00000000-0000-7000-8000-000000000538";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredGlobalCollection {
    pub(crate) item_id: String,
    pub(crate) title: String,
    pub(crate) member_count: i64,
    pub(crate) poster_item_id: Option<String>,
    pub(crate) poster_image_tag: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredGlobalCollectionSource {
    pub(crate) work_item_id: String,
    pub(crate) source_item_id: String,
    pub(crate) library_id: String,
    pub(crate) library_name: String,
    pub(crate) title: String,
    pub(crate) media_source_id: Option<String>,
    pub(crate) edition_name: Option<String>,
    pub(crate) quality_label: Option<String>,
    pub(crate) container: Option<String>,
    pub(crate) preferred_source_item_id: Option<String>,
    pub(crate) preferred_media_source_id: Option<String>,
    pub(crate) poster_image_tag: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredEmbyCollection {
    pub(crate) collection_item_id: String,
    pub(crate) library_id: String,
    pub(crate) title: String,
}

pub(crate) struct NewCollection<'a> {
    pub(crate) library_id: &'a str,
    pub(crate) provider: &'a str,
    pub(crate) provider_id: &'a str,
    pub(crate) title: &'a str,
    pub(crate) overview: Option<&'a str>,
    pub(crate) poster_path: Option<&'a str>,
    pub(crate) backdrop_path: Option<&'a str>,
    pub(crate) member_provider_ids: &'a [(String, String, i64)],
}

#[derive(Clone, Debug)]
pub(crate) struct StoredMediaMetadata {
    pub(crate) library_id: String,
    pub(crate) item_type: String,
    pub(crate) title: String,
    pub(crate) original_title: Option<String>,
    pub(crate) overview: Option<String>,
    pub(crate) production_year: Option<i64>,
    pub(crate) premiere_date: Option<String>,
    pub(crate) last_air_date: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) original_language: Option<String>,
    pub(crate) rating: Option<f64>,
    pub(crate) provider_ids_json: Option<String>,
    pub(crate) metadata_scraper_id: Option<String>,
    pub(crate) identification_status: String,
    pub(crate) scraper_id: Option<String>,
    pub(crate) provenance_json: Option<String>,
    pub(crate) locked_fields_json: Option<String>,
    pub(crate) nfo_metadata_json: Option<String>,
    pub(crate) metadata_fingerprint: Option<Vec<u8>>,
    pub(crate) nfo_collection_reference_key: Option<String>,
    pub(crate) collection_reference_identity_key: Option<String>,
    pub(crate) series_item_id: Option<String>,
    pub(crate) series_title: Option<String>,
    pub(crate) series_production_year: Option<i64>,
    pub(crate) series_provider_name: Option<String>,
    pub(crate) series_provider_id: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct StoredMetadataCandidate {
    pub(crate) id: String,
    pub(crate) item_id: String,
    pub(crate) provider: String,
    pub(crate) provider_id: String,
    pub(crate) candidate_json: String,
    pub(crate) score: f64,
    pub(crate) status: String,
    pub(crate) expires_at: Option<i64>,
    pub(crate) item_title: String,
}

#[derive(Debug)]
pub(crate) struct StoredMetadataCapabilityAttempt {
    pub(crate) provider: String,
    pub(crate) provider_id: String,
    pub(crate) capability: String,
    pub(crate) status: String,
    pub(crate) next_retry_at: Option<i64>,
}

#[allow(dead_code)] // Storage tests cover the legacy completeness read projection.
#[derive(Debug)]
pub(crate) struct StoredItemMetadataCompleteness {
    pub(crate) item_id: String,
    pub(crate) capability: String,
    pub(crate) local_state: String,
    pub(crate) is_missing: Option<bool>,
    pub(crate) input_fingerprint: Option<Vec<u8>>,
    pub(crate) checked_at: Option<i64>,
    pub(crate) retry_after: Option<i64>,
    pub(crate) error: Option<String>,
    pub(crate) updated_at: i64,
}

pub(crate) struct MetadataCapabilityResult<'a> {
    pub(crate) capability: &'a str,
    pub(crate) has_data: bool,
}

#[derive(Debug)]
pub(crate) struct StoredMetadataImageAttempt {
    pub(crate) image_type: String,
    pub(crate) candidate_key: String,
    pub(crate) status: String,
}

#[derive(Debug)]
pub(crate) struct StoredThumbnailScraperRetry {
    pub(crate) item_id: String,
    pub(crate) status: String,
    pub(crate) attempt_count: u32,
    pub(crate) first_attempt_at: i64,
    pub(crate) next_retry_at: Option<i64>,
}

#[derive(Clone, Copy)]
pub(crate) struct NewMetadataCandidate<'a> {
    pub(crate) id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) provider: &'a str,
    pub(crate) provider_id: &'a str,
    pub(crate) candidate_json: &'a str,
    pub(crate) score: f64,
    pub(crate) expires_at: Option<i64>,
}

/// One local-NFO snapshot in a batched metadata write.
pub(crate) struct MediaItemNfoMetadataUpdate<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) json: Option<&'a str>,
    pub(crate) source_fingerprint: Option<&'a [u8]>,
}

pub(crate) struct MetadataImageUnavailable<'a> {
    pub(crate) image_type: &'a str,
    pub(crate) candidate_key: &'a str,
}

#[derive(Debug)]
pub(crate) struct StoredMetadataReidentifyJob {
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) processed_count: i64,
    pub(crate) total_count: i64,
    pub(crate) error: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
    pub(crate) mode: String,
    pub(crate) cancel_requested: bool,
    pub(crate) library_id: Option<String>,
    pub(crate) job_scope: String,
    pub(crate) pending_count: i64,
}

#[derive(Debug)]
pub(crate) struct StoredMetadataReidentifyItem {
    pub(crate) job_id: String,
    pub(crate) item_id: String,
    pub(crate) status: String,
    pub(crate) candidate_count: i64,
    pub(crate) error: Option<String>,
    pub(crate) updated_at: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredJobActivityItem {
    pub(crate) item_type: String,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
    pub(crate) title: String,
    pub(crate) series_title: Option<String>,
}

fn stored_metadata_reidentify_job(row: sqlx::any::AnyRow) -> StoredMetadataReidentifyJob {
    StoredMetadataReidentifyJob {
        id: row.get("id"),
        status: row.get("status"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        error: row.get("error"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
        mode: row.get("mode"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
        library_id: row.get("library_id"),
        job_scope: row.get("job_scope"),
        pending_count: row.get("pending_count"),
    }
}

fn stored_metadata_reidentify_item(row: sqlx::any::AnyRow) -> StoredMetadataReidentifyItem {
    StoredMetadataReidentifyItem {
        job_id: row.get("job_id"),
        item_id: row.get("item_id"),
        status: row.get("status"),
        candidate_count: row.get("candidate_count"),
        error: row.get("error"),
        updated_at: row.get("updated_at"),
    }
}

fn stored_metadata_candidate(row: sqlx::any::AnyRow) -> StoredMetadataCandidate {
    StoredMetadataCandidate {
        id: row.get("id"),
        item_id: row.get("item_id"),
        provider: row.get("provider"),
        provider_id: row.get("provider_id"),
        candidate_json: row.get("candidate_json"),
        score: row.get("score"),
        status: row.get("status"),
        expires_at: row.get("expires_at"),
        item_title: row.get("item_title"),
    }
}

fn stored_item_metadata_completeness(row: sqlx::any::AnyRow) -> StoredItemMetadataCompleteness {
    StoredItemMetadataCompleteness {
        item_id: row.get("item_id"),
        capability: row.get("capability"),
        local_state: row.get("local_state"),
        is_missing: row
            .get::<Option<i64>, _>("is_missing")
            .map(|value| value != 0),
        input_fingerprint: row.get("input_fingerprint"),
        checked_at: row.get("checked_at"),
        retry_after: row.get("retry_after"),
        error: row.get("error"),
        updated_at: row.get("updated_at"),
    }
}

fn stored_media_item(row: sqlx::any::AnyRow) -> StoredMediaItem {
    StoredMediaItem { id: row.get("id") }
}

#[derive(Debug)]
pub(crate) struct StoredCatalogRow {
    pub(crate) item_id: String,
    pub(crate) library_id: String,
    pub(crate) item_type: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) series_id: Option<String>,
    pub(crate) series_name: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
    pub(crate) title: String,
    pub(crate) sort_title: String,
    pub(crate) original_title: Option<String>,
    pub(crate) overview: Option<String>,
    pub(crate) production_year: Option<i64>,
    pub(crate) rating: Option<f64>,
    pub(crate) rating_source: Option<String>,
    pub(crate) runtime_ticks: Option<i64>,
    pub(crate) added_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) poster_image_tag: Option<String>,
    pub(crate) fanart_image_tag: Option<String>,
    pub(crate) thumb_image_tag: Option<String>,
    pub(crate) logo_image_tag: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) source_relative_path: Option<String>,
    pub(crate) source_kind: Option<String>,
    pub(crate) container: Option<String>,
    pub(crate) size: Option<i64>,
    pub(crate) external_url: Option<String>,
    pub(crate) edition_name: Option<String>,
    pub(crate) quality_label: Option<String>,
    pub(crate) bitrate: Option<i64>,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) is_default: Option<bool>,
    pub(crate) probe_status: Option<String>,
    pub(crate) stream_id: Option<String>,
    pub(crate) stream_index: Option<i64>,
    pub(crate) stream_type: Option<String>,
    pub(crate) codec: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) stream_title: Option<String>,
    pub(crate) stream_details_json: Option<String>,
    pub(crate) stream_is_external: Option<bool>,
    pub(crate) stream_is_default: Option<bool>,
    pub(crate) stream_is_forced: Option<bool>,
}

#[derive(Debug)]
pub(crate) struct StoredCatalogDetail {
    pub(crate) series_name: Option<String>,
    pub(crate) premiere_date: Option<String>,
    pub(crate) last_air_date: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) original_language: Option<String>,
    pub(crate) provider_ids_json: Option<String>,
    pub(crate) season_count: i64,
    pub(crate) episode_count: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredMediaChapter {
    pub(crate) source_id: String,
    pub(crate) start_position_ticks: i64,
    pub(crate) name: Option<String>,
    pub(crate) marker_type: String,
    pub(crate) chapter_index: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredUserItemState {
    pub(crate) position_ticks: i64,
    pub(crate) is_played: bool,
    pub(crate) is_favorite: bool,
    pub(crate) hide_from_resume: bool,
    pub(crate) play_count: i64,
    pub(crate) last_played_at: Option<i64>,
    pub(crate) version: i64,
}

pub(crate) struct NewPlaybackEvent<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) media_source_id: Option<&'a str>,
    pub(crate) play_session_id: &'a str,
    pub(crate) device_id: &'a str,
    pub(crate) client: Option<&'a str>,
    pub(crate) device_name: Option<&'a str>,
    pub(crate) client_version: Option<&'a str>,
    pub(crate) device_type: Option<&'a str>,
    pub(crate) remote_ip: Option<&'a str>,
    pub(crate) state: &'a str,
    pub(crate) position_ticks: i64,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) played_percent: i64,
    pub(crate) is_paused: bool,
}

pub(crate) struct NewWebPlaybackSession<'a> {
    pub(crate) id: &'a str,
    pub(crate) user_id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) media_source_id: Option<&'a str>,
    pub(crate) play_session_id: &'a str,
    pub(crate) tier: i64,
    pub(crate) plan: &'a str,
    pub(crate) temp_dir: Option<&'a str>,
    pub(crate) is_admin: bool,
    pub(crate) expires_at: i64,
    pub(crate) now: i64,
}

#[allow(dead_code)] // LUX-775 creates context before returning a signed direct ticket.
#[derive(Clone, Copy, Debug)]
pub(crate) struct NewEmbyDirectPlaybackTicket<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) play_session_id: &'a str,
    pub(crate) authorization_device_id: Option<&'a str>,
    pub(crate) authorization_client_name: Option<&'a str>,
    pub(crate) expires_at: i64,
    pub(crate) created_at: i64,
}

#[allow(dead_code)] // LUX-776 enforces blacklist rules using this stored context.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredEmbyDirectPlaybackTicket {
    pub(crate) user_id: String,
    pub(crate) play_session_id: String,
    pub(crate) authorization_device_id: Option<String>,
    pub(crate) authorization_client_name: Option<String>,
    pub(crate) expires_at: i64,
    pub(crate) revoked_at: Option<i64>,
    pub(crate) created_at: i64,
}

pub(crate) struct NewWebPlaybackEvent<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) user_id: &'a str,
    pub(crate) event_id: &'a str,
    pub(crate) sequence: i64,
    pub(crate) state: &'a str,
    pub(crate) position_ticks: i64,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) now: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredWebPlaybackAccessContext {
    pub(crate) user_id: String,
    pub(crate) device_id: Option<String>,
    pub(crate) client_name: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct WebPlaybackTranscodingDetails {
    pub(crate) video_codec: Option<String>,
    pub(crate) audio_codec: Option<String>,
    pub(crate) video_bitrate: Option<i64>,
    pub(crate) audio_bitrate: Option<i64>,
    pub(crate) transcoding_container: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WebPlaybackEventClaim {
    Accepted,
    Duplicate,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredWebPlaybackSession {
    pub(crate) id: String,
    pub(crate) user_id: String,
    pub(crate) item_id: String,
    pub(crate) media_source_id: Option<String>,
    pub(crate) play_session_id: String,
    pub(crate) tier: i64,
    pub(crate) plan: String,
    pub(crate) state: String,
    pub(crate) temp_dir: Option<String>,
    pub(crate) is_admin: bool,
    pub(crate) expires_at: i64,
    pub(crate) last_heartbeat_at: i64,
    pub(crate) last_sequence: i64,
    pub(crate) video_codec: Option<String>,
    pub(crate) audio_codec: Option<String>,
    pub(crate) video_bitrate: Option<i64>,
    pub(crate) audio_bitrate: Option<i64>,
    pub(crate) transcoding_container: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

fn stored_web_playback_session(row: sqlx::any::AnyRow) -> StoredWebPlaybackSession {
    StoredWebPlaybackSession {
        id: row.get("id"),
        user_id: row.get("user_id"),
        item_id: row.get("item_id"),
        media_source_id: row.get("media_source_id"),
        play_session_id: row.get("play_session_id"),
        tier: row.get("tier"),
        plan: row.get("plan"),
        state: row.get("state"),
        temp_dir: row.get("temp_dir"),
        is_admin: row.get::<i64, _>("is_admin") != 0,
        expires_at: row.get("expires_at"),
        last_heartbeat_at: row.get("last_heartbeat_at"),
        last_sequence: row.get("last_sequence"),
        video_codec: row.get("video_codec"),
        audio_codec: row.get("audio_codec"),
        video_bitrate: row.get("video_bitrate"),
        audio_bitrate: row.get("audio_bitrate"),
        transcoding_container: row.get("transcoding_container"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredPlaybackSession {
    pub(crate) id: String,
    pub(crate) user_id: String,
    pub(crate) item_id: String,
    pub(crate) media_source_id: Option<String>,
    pub(crate) play_session_id: String,
    pub(crate) device_id: String,
    pub(crate) client: Option<String>,
    pub(crate) device_name: Option<String>,
    pub(crate) client_version: Option<String>,
    pub(crate) device_type: Option<String>,
    pub(crate) remote_ip: Option<String>,
    pub(crate) state: String,
    pub(crate) position_ticks: i64,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) is_paused: bool,
    pub(crate) started_at: i64,
    pub(crate) last_event_at: i64,
}

fn stored_playback_session(row: sqlx::any::AnyRow) -> StoredPlaybackSession {
    StoredPlaybackSession {
        id: row.get("id"),
        user_id: row.get("user_id"),
        item_id: row.get("item_id"),
        media_source_id: row.get("media_source_id"),
        play_session_id: row.get("play_session_id"),
        device_id: row.get("device_id"),
        client: row.get("client"),
        device_name: row.get("device_name"),
        client_version: row.get("client_version"),
        device_type: row.get("device_type"),
        remote_ip: row.get("remote_ip"),
        state: row.get("state"),
        position_ticks: row.get("position_ticks"),
        duration_ticks: row.get("duration_ticks"),
        is_paused: row.get::<i64, _>("is_paused") != 0,
        started_at: row.get("started_at"),
        last_event_at: row.get("last_event_at"),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredExternalSubtitle {
    pub(crate) media_source_id: String,
    pub(crate) item_id: String,
    pub(crate) external_path: String,
    pub(crate) language: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) root_path: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredSubtitleStream {
    pub(crate) media_source_id: String,
    pub(crate) item_id: String,
    pub(crate) source_kind: String,
    pub(crate) probe_status: String,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
    pub(crate) stream_index: i64,
    pub(crate) stream_type: String,
    pub(crate) codec: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) details_json: Option<String>,
    pub(crate) external_path: Option<String>,
    pub(crate) is_external: bool,
    pub(crate) is_default: bool,
    pub(crate) is_forced: bool,
}

#[derive(Debug)]
pub(crate) struct StoredItemImageCandidate {
    pub(crate) id: String,
    pub(crate) image_type: String,
    pub(crate) local_path: String,
    pub(crate) root_path: String,
    pub(crate) content_tag: Option<String>,
    pub(crate) width: Option<i64>,
    pub(crate) height: Option<i64>,
}

pub(crate) struct ItemImageMetadata<'a> {
    pub(crate) file_size: i64,
    pub(crate) width: Option<i32>,
    pub(crate) height: Option<i32>,
    pub(crate) content_tag: &'a str,
    pub(crate) source: &'a str,
    pub(crate) source_url: Option<&'a str>,
}

pub(crate) struct ItemImageInsert {
    pub(crate) image_type: String,
    pub(crate) image_index: i64,
    pub(crate) local_path: String,
    pub(crate) file_size: i64,
    pub(crate) width: Option<i32>,
    pub(crate) height: Option<i32>,
    pub(crate) content_tag: String,
    pub(crate) source: String,
    pub(crate) source_url: Option<String>,
}

pub(crate) struct ItemImageBatchInsert {
    pub(crate) item_id: String,
    pub(crate) images: Vec<ItemImageInsert>,
    pub(crate) clear_poster_fallback: bool,
}

pub(crate) struct MetadataImageAttemptUpdate<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) image_type: &'a str,
    pub(crate) candidate_key: &'a str,
    pub(crate) status: &'a str,
    pub(crate) next_retry_at: Option<i64>,
    pub(crate) error_code: Option<&'a str>,
    pub(crate) now: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredItemImage {
    pub(crate) id: String,
    pub(crate) item_id: String,
    pub(crate) image_type: String,
    pub(crate) image_index: i64,
    pub(crate) local_path: String,
    pub(crate) file_size: Option<i64>,
    pub(crate) content_tag: Option<String>,
    pub(crate) source: String,
    pub(crate) root_path: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredItemImagePathConflict {
    pub(crate) id: String,
    pub(crate) item_id: String,
    pub(crate) image_type: String,
    pub(crate) local_path: String,
}

#[derive(Debug)]
pub(crate) struct StoredCatalogImageTag {
    pub(crate) id: String,
    pub(crate) image_type: String,
    pub(crate) image_index: i64,
}

#[derive(Debug)]
pub(crate) struct StoredLibraryPoster {
    pub(crate) item_id: String,
    pub(crate) local_path: String,
    pub(crate) root_path: String,
}

fn stored_item_image(row: sqlx::any::AnyRow) -> StoredItemImage {
    StoredItemImage {
        id: row.get("id"),
        item_id: row.get("item_id"),
        image_type: row.get("image_type"),
        image_index: row.get("image_index"),
        local_path: row.get("local_path"),
        file_size: row.get("file_size"),
        content_tag: row.get("content_tag"),
        source: row.get("source"),
        root_path: row.get("root_path"),
    }
}

fn catalog_filter_where_clause<'a>(
    filter: &CatalogFilterQuery<'a>,
) -> (String, Vec<CatalogBind<'a>>) {
    let library_ids = filter.library_ids;
    let user_id = filter.user_id;
    let item_types = filter.item_types;
    let item_ids = filter.item_ids;
    let person_id = filter.person_id;
    let media_source_ids = filter.media_source_ids;
    let provider_id_equals = filter.provider_id_equals;
    let excluded_item_types = filter.excluded_item_types;
    let years = filter.years;
    let is_played = filter.is_played;
    let is_favorite = filter.is_favorite;
    let min_date_last_saved = filter.min_date_last_saved;
    let metadata_pending = filter.metadata_pending;
    let mut where_clause = format!(
        "WHERE mi.removed_at IS NULL
         AND mi.library_id IN ({})",
        std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut binds = library_ids
        .iter()
        .map(|library_id| CatalogBind::Text(library_id.as_str()))
        .collect::<Vec<_>>();
    match filter.parent_id_scope {
        Some(Some(parent_id)) => {
            where_clause.push_str(
                " AND mi.parent_id = ?
                  AND EXISTS (
                      SELECT 1 FROM media_items parent
                      WHERE parent.id = ?
                        AND parent.library_id = mi.library_id
                        AND parent.item_type = 'FOLDER'
                        AND parent.removed_at IS NULL
                        AND parent.merged_into_item_id IS NULL
                  )",
            );
            binds.push(CatalogBind::Text(parent_id));
            binds.push(CatalogBind::Text(parent_id));
        }
        Some(None) => {
            where_clause.push_str(" AND (mi.parent_id IS NULL OR mi.parent_id = mi.library_id)")
        }
        None => {}
    }
    if item_types.is_empty() {
        where_clause.push_str(" AND mi.item_type <> 'FOLDER'");
    }
    if filter.parent_id_scope.is_some() {
        where_clause.push_str(CATALOG_FOLDER_BROWSE_VISIBLE_PREDICATE);
    } else {
        where_clause.push_str(CATALOG_VISIBLE_PREDICATE);
    }
    append_content_restriction_predicate(
        &mut where_clause,
        &mut binds,
        filter.content_restrictions,
    );
    let mut id_predicates = Vec::new();
    if let Some(item_ids) = item_ids
        && !item_ids.is_empty()
    {
        id_predicates.push(format!(
            "mi.id IN ({})",
            std::iter::repeat_n("?", item_ids.len())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        binds.extend(
            item_ids
                .iter()
                .map(|item_id| CatalogBind::Text(item_id.as_str())),
        );
    }
    if let Some(media_source_ids) = media_source_ids
        && !media_source_ids.is_empty()
    {
        id_predicates.push(format!(
            "EXISTS (SELECT 1 FROM media_sources ms_filter
                     WHERE ms_filter.item_id = mi.id AND ms_filter.id IN ({}))",
            std::iter::repeat_n("?", media_source_ids.len())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        binds.extend(
            media_source_ids
                .iter()
                .map(|media_source_id| CatalogBind::Text(media_source_id.as_str())),
        );
    }
    if item_ids.is_some() || media_source_ids.is_some() {
        if id_predicates.is_empty() {
            where_clause.push_str(" AND 1 = 0");
        } else {
            where_clause.push_str(&format!(" AND ({})", id_predicates.join(" OR ")));
        }
    }
    if let Some(provider_id_equals) = provider_id_equals {
        if provider_id_equals.is_empty() {
            where_clause.push_str(" AND 1 = 0");
        } else {
            let predicates = std::iter::repeat_n(
                "(provider_filter.provider = ? AND provider_filter.provider_id = ?)",
                provider_id_equals.len(),
            )
            .collect::<Vec<_>>()
            .join(" OR ");
            where_clause.push_str(&format!(
                " AND EXISTS (
                     SELECT 1
                     FROM media_item_provider_ids provider_filter
                     WHERE provider_filter.media_item_id = mi.id
                       AND ({predicates})
                 )"
            ));
            for (provider, provider_id) in provider_id_equals {
                binds.push(CatalogBind::Text(provider.as_str()));
                binds.push(CatalogBind::Text(provider_id.as_str()));
            }
        }
    }
    if let Some(person_id) = person_id {
        where_clause.push_str(
            " AND EXISTS (
                 SELECT 1
                 FROM person_credits person_filter
                 JOIN media_items credit_item ON credit_item.id = person_filter.item_id
                 LEFT JOIN person_identities identity_filter
                   ON identity_filter.provider = person_filter.provider
                  AND identity_filter.provider_id = person_filter.person_id
                 WHERE person_filter.person_type = ?
                   AND (
                       person_filter.person_id = ?
                       OR person_filter.lux_person_id = ?
                       OR identity_filter.person_id = ?
                   )
                   AND credit_item.removed_at IS NULL
                   AND (
                       credit_item.has_available_source = 1
                       OR (
                           credit_item.item_type IN ('SERIES', 'SEASON', 'BOX_SET', 'FOLDER')
                           AND EXISTS (
                               SELECT 1
                               FROM media_items visible_credit_child
                               WHERE visible_credit_child.removed_at IS NULL
                                 AND visible_credit_child.has_available_source = 1
                                 AND (
                                     visible_credit_child.parent_id = credit_item.id
                                     OR visible_credit_child.series_id = credit_item.id
                                 )
                           )
                       )
                   )
                   AND (
                       person_filter.item_id = mi.id
                       OR credit_item.series_id = mi.id
                       OR EXISTS (
                           SELECT 1
                           FROM media_items credit_parent
                           WHERE credit_parent.id = credit_item.parent_id
                             AND (
                                 credit_parent.series_id = mi.id
                                 OR credit_parent.parent_id = mi.id
                             )
                       )
                   )
             )",
        );
        binds.push(CatalogBind::Text("Actor"));
        binds.push(CatalogBind::Text(person_id));
        binds.push(CatalogBind::Text(person_id));
        binds.push(CatalogBind::Text(person_id));
    }
    if !item_types.is_empty() {
        where_clause.push_str(&format!(
            " AND mi.item_type IN ({})",
            std::iter::repeat_n("?", item_types.len())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        binds.extend(
            item_types
                .iter()
                .map(|item_type| CatalogBind::Text(item_type.as_str())),
        );
    }
    if !excluded_item_types.is_empty() {
        where_clause.push_str(&format!(
            " AND mi.item_type NOT IN ({})",
            std::iter::repeat_n("?", excluded_item_types.len())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        binds.extend(
            excluded_item_types
                .iter()
                .map(|item_type| CatalogBind::Text(item_type.as_str())),
        );
    }
    if !years.is_empty() {
        where_clause.push_str(&format!(
            " AND mi.production_year IN ({})",
            std::iter::repeat_n("?", years.len())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        binds.extend(years.iter().copied().map(CatalogBind::Integer));
    }
    if let Some(is_played) = is_played {
        where_clause.push_str(
            " AND COALESCE(
                (SELECT state_filter.is_played
                 FROM user_item_state state_filter
                 WHERE state_filter.user_id = ? AND state_filter.item_id = mi.id),
                0
            ) = ?",
        );
        binds.push(CatalogBind::Text(user_id));
        binds.push(CatalogBind::Integer(i64::from(is_played)));
    }
    if let Some(is_favorite) = is_favorite {
        if is_favorite {
            where_clause.push_str(
                " AND mi.id IN (
                    SELECT state_filter.item_id
                    FROM user_item_state state_filter
                    WHERE state_filter.user_id = ? AND state_filter.is_favorite = ?
                )",
            );
        } else {
            where_clause.push_str(
                " AND COALESCE(
                    (SELECT state_filter.is_favorite
                     FROM user_item_state state_filter
                     WHERE state_filter.user_id = ? AND state_filter.item_id = mi.id),
                    0
                ) = ?",
            );
        }
        binds.push(CatalogBind::Text(user_id));
        binds.push(CatalogBind::Integer(i64::from(is_favorite)));
    }
    if metadata_pending {
        where_clause.push_str(
            " AND EXISTS (
                SELECT 1 FROM metadata_candidates pending_metadata
                WHERE pending_metadata.item_id = mi.id
                  AND pending_metadata.status = 'PENDING'
            )",
        );
    }
    if let Some(min_date_last_saved) = min_date_last_saved {
        where_clause.push_str(" AND mi.updated_at >= ?");
        binds.push(CatalogBind::Integer(min_date_last_saved));
    }
    (where_clause, binds)
}

fn content_rating_order_expression(rating_system: &str) -> Option<&'static str> {
    match rating_system {
        "MPA" => Some(
            "CASE restriction_index.certification_code
                 WHEN 'G' THEN 0 WHEN 'PG' THEN 1 WHEN 'PG-13' THEN 2
                 WHEN 'R' THEN 3 WHEN 'NC-17' THEN 4 ELSE NULL END",
        ),
        "TVPG" => Some(
            "CASE restriction_index.certification_code
                 WHEN 'TV-Y' THEN 0 WHEN 'TV-G' THEN 1 WHEN 'TV-Y7' THEN 2
                 WHEN 'TV-PG' THEN 3 WHEN 'TV-14' THEN 4 WHEN 'TV-MA' THEN 5
                 ELSE NULL END",
        ),
        "BBFC" => Some(
            "CASE restriction_index.certification_code
                 WHEN 'U' THEN 0 WHEN 'PG' THEN 1 WHEN '12' THEN 2
                 WHEN '12A' THEN 2 WHEN '15' THEN 3 WHEN '18' THEN 4
                 WHEN 'R18' THEN 5 ELSE NULL END",
        ),
        _ => None,
    }
}

fn content_rating_order(rating_system: &str, rating: &str) -> Option<i64> {
    let levels: &[&str] = match rating_system {
        "MPA" => &["G", "PG", "PG-13", "R", "NC-17"],
        "TVPG" => &["TV-Y", "TV-G", "TV-Y7", "TV-PG", "TV-14", "TV-MA"],
        "BBFC" => &["U", "PG", "12", "12A", "15", "18", "R18"],
        _ => return None,
    };
    if rating_system == "BBFC" && rating.eq_ignore_ascii_case("12A") {
        return Some(2);
    }
    levels
        .iter()
        .position(|level| level.eq_ignore_ascii_case(rating))
        .map(|order| order as i64)
}

fn append_content_restriction_predicate<'a>(
    where_clause: &mut String,
    binds: &mut Vec<CatalogBind<'a>>,
    restrictions: Option<&'a UserContentRestrictions>,
) {
    append_content_restriction_predicate_for_item_alias(where_clause, binds, restrictions, "mi");
}

fn append_content_restriction_predicate_for_item_alias<'a>(
    where_clause: &mut String,
    binds: &mut Vec<CatalogBind<'a>>,
    restrictions: Option<&'a UserContentRestrictions>,
    item_alias: &str,
) {
    let Some(restrictions) = restrictions else {
        return;
    };
    let policy = ContentRestrictionPolicy {
        rating_system: &restrictions.rating_system,
        maximum_rating: restrictions.maximum_rating.as_deref(),
        allow_unrated: restrictions.allow_unrated,
        allowed_tags: &restrictions.allowed_tags,
        blocked_tags: &restrictions.blocked_tags,
    };
    if !content_restriction_policy_is_valid(&policy) {
        where_clause.push_str(" AND 1 = 0");
        return;
    }
    if !content_restriction_policy_is_restrictive(&policy) {
        return;
    }

    let mut violations = vec!["restriction_index.item_id IS NULL".to_owned()];
    if restrictions.maximum_rating.is_some() || !restrictions.allow_unrated {
        let Some(rating_order_expression) =
            content_rating_order_expression(&restrictions.rating_system)
        else {
            where_clause.push_str(" AND 1 = 0");
            return;
        };
        if let Some(maximum_rating) = restrictions.maximum_rating.as_deref() {
            let Some(maximum_order) =
                content_rating_order(&restrictions.rating_system, maximum_rating)
            else {
                where_clause.push_str(" AND 1 = 0");
                return;
            };
            let unrated_disallowed = !restrictions.allow_unrated;
            violations.push(format!(
                "(({rating_order_expression}) IS NOT NULL
                    AND ({rating_order_expression}) > {maximum_order})
                 OR (({rating_order_expression}) IS NULL AND {unrated_disallowed})"
            ));
        } else {
            violations.push(format!("({rating_order_expression}) IS NULL"));
        }
    }

    if !restrictions.blocked_tags.is_empty() {
        let placeholders = std::iter::repeat_n("?", restrictions.blocked_tags.len())
            .collect::<Vec<_>>()
            .join(", ");
        violations.push(format!(
            "EXISTS (
                SELECT 1 FROM media_content_restriction_tags blocked_tag
                WHERE blocked_tag.item_id = restriction_item.id
                  AND blocked_tag.tag IN ({placeholders})
            )"
        ));
        binds.extend(
            restrictions
                .blocked_tags
                .iter()
                .map(|tag| CatalogBind::Text(tag)),
        );
    }
    if !restrictions.allowed_tags.is_empty() {
        let placeholders = std::iter::repeat_n("?", restrictions.allowed_tags.len())
            .collect::<Vec<_>>()
            .join(", ");
        violations.push(format!(
            "NOT EXISTS (
                SELECT 1 FROM media_content_restriction_tags allowed_tag
                WHERE allowed_tag.item_id = restriction_item.id
                  AND allowed_tag.tag IN ({placeholders})
            )"
        ));
        binds.extend(
            restrictions
                .allowed_tags
                .iter()
                .map(|tag| CatalogBind::Text(tag)),
        );
    }

    where_clause.push_str(&format!(
        " AND NOT EXISTS (
             SELECT 1
             FROM media_items restriction_item
             LEFT JOIN media_content_restriction_index restriction_index
               ON restriction_index.item_id = restriction_item.id
             WHERE restriction_item.id IN ({item_alias}.id, {item_alias}.parent_id, {item_alias}.series_id)
               AND restriction_item.item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE', 'VIDEO')
               AND (",
    ));
    where_clause.push_str(&violations.join(" OR "));
    where_clause.push_str("))");
}

pub(crate) fn movie_parent_folder_identity(
    library_root_id: &str,
    relative_path: &str,
) -> Option<String> {
    let directory = relative_path
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .or_else(|| {
            relative_path
                .rsplit_once('\\')
                .map(|(directory, _)| directory)
        })
        .unwrap_or_default();
    let mut directory_key = String::new();
    for component in directory.split(['/', '\\']) {
        if component.is_empty() || component == "." {
            continue;
        }
        if !directory_key.is_empty() {
            directory_key.push('/');
        }
        directory_key.push_str(component);
    }
    (!directory_key.is_empty()).then(|| format!("folder:{library_root_id}:{directory_key}"))
}

const CATALOG_VISIBLE_PREDICATE: &str = " AND mi.merged_into_item_id IS NULL
 AND (
    mi.has_available_source = 1
    OR (
        mi.item_type IN ('SERIES', 'SEASON', 'BOX_SET', 'FOLDER')
        AND EXISTS (
            SELECT 1
            FROM media_items visible_child
            WHERE visible_child.removed_at IS NULL
              AND visible_child.merged_into_item_id IS NULL
              AND visible_child.has_available_source = 1
              AND (visible_child.parent_id = mi.id OR visible_child.series_id = mi.id)
        )
        OR EXISTS (
            SELECT 1
            FROM collection_items visible_collection_item
            JOIN collections visible_collection
              ON visible_collection.id = visible_collection_item.collection_id
            JOIN media_items visible_child
              ON visible_child.id = visible_collection_item.item_id
            WHERE visible_collection.item_id = mi.id
              AND visible_child.removed_at IS NULL
              AND visible_child.merged_into_item_id IS NULL
              AND visible_child.has_available_source = 1
        )
    )
)";

const CATALOG_FOLDER_BROWSE_VISIBLE_PREDICATE: &str = " AND mi.merged_into_item_id IS NULL
 AND (
    (mi.item_type = 'VIDEO' AND mi.has_available_source = 1)
    OR (
        mi.item_type = 'FOLDER'
        AND EXISTS (
            WITH RECURSIVE folder_descendants(id) AS (
                SELECT child.id
                FROM media_items child
                WHERE child.parent_id = mi.id
                  AND child.item_type = 'FOLDER'
                  AND child.removed_at IS NULL
                  AND child.merged_into_item_id IS NULL
                UNION
                SELECT child.id
                FROM media_items child
                JOIN folder_descendants parent ON child.parent_id = parent.id
                WHERE child.item_type = 'FOLDER'
                  AND child.removed_at IS NULL
                  AND child.merged_into_item_id IS NULL
            )
            SELECT 1
            FROM media_items visible_descendant
            WHERE (visible_descendant.parent_id = mi.id
                   OR visible_descendant.parent_id IN (SELECT id FROM folder_descendants))
              AND visible_descendant.item_type = 'VIDEO'
              AND visible_descendant.removed_at IS NULL
              AND visible_descendant.merged_into_item_id IS NULL
              AND visible_descendant.has_available_source = 1
        )
    )
)";

fn resume_runtime_ticks_sql() -> &'static str {
    "COALESCE(
        NULLIF(mi.runtime_ticks, 0),
        (
            SELECT ms_default.duration_ticks
            FROM media_sources ms_default
            JOIN filesystem_entries fe_default
              ON fe_default.id = ms_default.filesystem_entry_id
             AND fe_default.is_missing = 0
            WHERE ms_default.item_id = mi.id
              AND ms_default.is_default = 1
              AND ms_default.duration_ticks > 0
            ORDER BY ms_default.id
            LIMIT 1
        ),
        (
            SELECT ms_first.duration_ticks
            FROM media_sources ms_first
            JOIN filesystem_entries fe_first
              ON fe_first.id = ms_first.filesystem_entry_id
             AND fe_first.is_missing = 0
            WHERE ms_first.item_id = mi.id
              AND ms_first.duration_ticks > 0
            ORDER BY ms_first.id
            LIMIT 1
        )
    )"
}

#[derive(Clone, Copy)]
enum CatalogBind<'a> {
    Text(&'a str),
    Integer(i64),
    Real(f64),
}

#[derive(Clone, Copy)]
pub enum PersonSort {
    Name,
    DateCreated,
}

#[derive(Clone, Copy)]
pub struct PersonListOptions {
    pub recursive: bool,
    pub sort_by: PersonSort,
    pub descending: bool,
    pub offset: i64,
    pub limit: i64,
}

fn person_sort_order(sort_by: PersonSort, descending: bool) -> String {
    let direction = if descending { "DESC" } else { "ASC" };
    match sort_by {
        PersonSort::Name => format!(
            "representative.person_name {direction}, representative.date_created DESC, representative.provider ASC, representative.person_id ASC"
        ),
        PersonSort::DateCreated => format!(
            "representative.date_created {direction}, representative.person_name ASC, representative.provider ASC, representative.person_id ASC"
        ),
    }
}

pub(crate) struct CatalogFilterQuery<'a> {
    pub(crate) library_ids: &'a [String],
    pub(crate) user_id: &'a str,
    pub(crate) item_types: &'a [String],
    pub(crate) excluded_item_types: &'a [String],
    pub(crate) item_ids: Option<&'a [String]>,
    pub(crate) person_id: Option<&'a str>,
    pub(crate) media_source_ids: Option<&'a [String]>,
    pub(crate) provider_id_equals: Option<&'a [(String, String)]>,
    pub(crate) years: &'a [i64],
    pub(crate) is_played: Option<bool>,
    pub(crate) is_favorite: Option<bool>,
    pub(crate) min_date_last_saved: Option<i64>,
    pub(crate) metadata_pending: bool,
    pub(crate) parent_id_scope: Option<Option<&'a str>>,
    pub(crate) content_restrictions: Option<&'a UserContentRestrictions>,
    pub(crate) sort_by: CatalogSort,
    pub(crate) descending: bool,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
    pub(crate) include_total: bool,
}

pub(crate) struct ResumeItemsQuery<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) library_ids: &'a [String],
    pub(crate) item_types: &'a [&'a str],
    pub(crate) content_restrictions: Option<&'a UserContentRestrictions>,
    pub(crate) played_percent: i64,
    pub(crate) minimum_ticks: i64,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CatalogSort {
    Name,
    SortName,
    DateCreated,
    PremiereDate,
    ProductionYear,
    Rating,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredMediaSourcePath {
    pub(crate) source_id: String,
    pub(crate) item_id: String,
    pub(crate) probe_status: String,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredMediaWritebackContext {
    pub(crate) item_type: String,
    pub(crate) source: Option<StoredMediaSourcePath>,
}

#[derive(Debug)]
pub(crate) struct StoredItemScanPath {
    pub(crate) library_id: String,
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
}

#[derive(Debug)]
pub(crate) struct StoredFolderScanPath {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) title: String,
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
}

pub(crate) struct StoredPlaybackSource {
    pub(crate) source_id: String,
    pub(crate) source_kind: String,
    pub(crate) container: Option<String>,
    pub(crate) external_url: Option<String>,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredDanmakuSource {
    pub(crate) source_id: String,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
    pub(crate) item_type: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
    pub(crate) title: Option<String>,
    pub(crate) original_title: Option<String>,
    pub(crate) series_title: Option<String>,
    pub(crate) series_original_title: Option<String>,
}

pub(crate) struct NewDanmakuTrack<'a> {
    pub(crate) id: &'a str,
    pub(crate) media_source_id: &'a str,
    pub(crate) relative_path: &'a str,
    pub(crate) provider: Option<&'a str>,
    pub(crate) provider_anime_id: Option<&'a str>,
    pub(crate) provider_episode_id: Option<&'a str>,
    pub(crate) fingerprint: Option<&'a [u8]>,
    pub(crate) status: &'a str,
    pub(crate) error_code: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredDanmakuMatchJob {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) status: String,
    pub(crate) overwrite: bool,
    pub(crate) concurrency: i64,
    pub(crate) total_count: i64,
    pub(crate) processed_count: i64,
    pub(crate) success_count: i64,
    pub(crate) skipped_count: i64,
    pub(crate) failed_count: i64,
    pub(crate) cancel_requested: bool,
    pub(crate) error: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredDanmakuMatchItem {
    pub(crate) id: String,
    pub(crate) media_source_id: String,
    pub(crate) root_path: Option<String>,
    pub(crate) relative_path: Option<String>,
    pub(crate) item_type: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
    pub(crate) title: Option<String>,
    pub(crate) original_title: Option<String>,
    pub(crate) series_title: Option<String>,
    pub(crate) series_original_title: Option<String>,
}

pub(crate) struct NewDanmakuMatchJob<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) overwrite: bool,
    pub(crate) concurrency: i64,
}

#[derive(Debug)]
pub(crate) struct StoredThumbnailSource {
    pub(crate) item_id: String,
    pub(crate) item_type: String,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
    pub(crate) library_media_strategy_json: Option<String>,
    pub(crate) scraper_id: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredStrmMediaSource {
    pub(crate) source_id: String,
    pub(crate) item_id: String,
    pub(crate) poster_fallback_required: bool,
    pub(crate) has_media_info: bool,
    pub(crate) external_url: Option<String>,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
    pub(crate) thumbnail_path: Option<String>,
    pub(crate) has_strm_thumbnail: bool,
}

#[derive(Debug)]
pub(crate) struct StoredImageIdentity {
    pub(crate) item_type: String,
    pub(crate) original_language: Option<String>,
    pub(crate) provider_name: Option<String>,
    pub(crate) provider_id: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct StoredMovieIdentity {
    pub(crate) library_id: String,
    pub(crate) provider_name: String,
    pub(crate) provider_id: String,
}

fn stored_danmaku_match_job(row: sqlx::any::AnyRow) -> StoredDanmakuMatchJob {
    StoredDanmakuMatchJob {
        id: row.get("id"),
        library_id: row.get("library_id"),
        status: row.get("status"),
        overwrite: row.get::<i64, _>("overwrite") != 0,
        concurrency: row.get("concurrency"),
        total_count: row.get("total_count"),
        processed_count: row.get("processed_count"),
        success_count: row.get("success_count"),
        skipped_count: row.get("skipped_count"),
        failed_count: row.get("failed_count"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
        error: row.get("error"),
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
    }
}

fn first_provider_id(
    primary: Option<String>,
    secondary: Option<String>,
    preferred: Option<&str>,
) -> Option<(String, String)> {
    let providers = [primary, secondary]
        .into_iter()
        .flatten()
        .filter_map(|raw| {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw).ok()
        })
        .flat_map(|object| object.into_iter())
        .filter_map(|(name, value)| {
            let id = value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_i64().map(|value| value.to_string()))?;
            (!name.trim().is_empty() && !id.trim().is_empty()).then_some((name, id))
        })
        .collect::<Vec<_>>();
    if let Some(preferred) = preferred {
        let short_preferred = preferred
            .rsplit(['.', ':', '/'])
            .next()
            .unwrap_or(preferred);
        providers
            .iter()
            .find(|(name, _)| {
                name.eq_ignore_ascii_case(preferred) || name.eq_ignore_ascii_case(short_preferred)
            })
            .cloned()
    } else {
        providers.into_iter().next()
    }
}

#[derive(Debug)]
pub(crate) struct StoredMediaItemKind {
    pub(crate) item_type: String,
    pub(crate) season_number: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct StoredSeriesMetadataSource {
    pub(crate) series_id: String,
    pub(crate) season_id: String,
    pub(crate) episode_id: String,
    pub(crate) season_number: Option<i64>,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
}

#[derive(Debug)]
pub(crate) struct StoredScanJobMetadataPage {
    pub(crate) job_type: Option<String>,
    pub(crate) job_status: Option<String>,
    pub(crate) auto_metadata_match: bool,
    pub(crate) has_pending: bool,
    pub(crate) sources: StoredScanJobMetadataSources,
}

#[derive(Debug)]
pub(crate) enum StoredScanJobMetadataSources {
    None,
    Movies(Vec<StoredMediaSourcePath>),
    HomeVideos(Vec<StoredMediaSourcePath>),
    Episodes(Vec<StoredSeriesMetadataSource>),
}

#[derive(Debug)]
pub(crate) struct StoredScanManifestPostprocessingState {
    pub(crate) manifest_id: String,
    pub(crate) workflow_version: i64,
    pub(crate) discovery_format_version: i64,
    pub(crate) roots: Vec<StoredScanManifestPostprocessingRoot>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredChapterDetectionSource {
    pub(crate) source_id: String,
    pub(crate) item_id: String,
    pub(crate) season_id: String,
    pub(crate) fingerprint: Option<Vec<u8>>,
    pub(crate) relative_path: String,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) provider_ids_json: Option<String>,
    pub(crate) series_provider_ids_json: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
    pub(crate) state: Option<StoredChapterDetectionSourceState>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredChapterDetectionSourceState {
    pub(crate) input_fingerprint: Vec<u8>,
    pub(crate) status: String,
    pub(crate) last_checked_at: i64,
    pub(crate) next_retry_at: Option<i64>,
    pub(crate) intro_fingerprint: Option<Vec<u8>>,
    pub(crate) credits_fingerprint: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredChapterDetectionItem {
    pub(crate) source_id: String,
    pub(crate) season_id: String,
    pub(crate) source_fingerprint: Option<Vec<u8>>,
    pub(crate) input_fingerprint: Vec<u8>,
    pub(crate) is_context: bool,
    pub(crate) intro_fingerprint: Option<Vec<u8>>,
    pub(crate) credits_fingerprint: Option<Vec<u8>>,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
    pub(crate) provider_ids_json: Option<String>,
    pub(crate) series_provider_ids_json: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredChapterDetectionJob {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) plugin_id: String,
    pub(crate) status: String,
    pub(crate) concurrency: i64,
    pub(crate) intro_window_seconds: i64,
    pub(crate) credits_window_seconds: i64,
    pub(crate) match_threshold: f64,
    pub(crate) cursor: Option<String>,
    pub(crate) processed_count: i64,
    pub(crate) total_count: i64,
    pub(crate) cancel_requested: bool,
    pub(crate) error: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
}

pub(crate) struct NewChapterDetectionJob<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) plugin_id: &'a str,
    pub(crate) concurrency: i64,
    pub(crate) intro_window_seconds: i64,
    pub(crate) credits_window_seconds: i64,
    pub(crate) match_threshold: f64,
    pub(crate) total_count: i64,
}

pub(crate) struct NewChapterDetectionJobItem<'a> {
    pub(crate) job_id: &'a str,
    pub(crate) source_id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) season_id: &'a str,
    pub(crate) source_fingerprint: &'a [u8],
    pub(crate) input_fingerprint: &'a [u8],
    pub(crate) is_context: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredLibraryCoverJob {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) is_manual: bool,
    pub(crate) status: String,
    pub(crate) processed_count: i64,
    pub(crate) total_count: i64,
    pub(crate) error: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
}

fn stored_library_cover_job(row: sqlx::any::AnyRow) -> StoredLibraryCoverJob {
    StoredLibraryCoverJob {
        id: row.get("id"),
        library_id: row.get("library_id"),
        is_manual: row.get::<i64, _>("is_manual") != 0,
        status: row.get("status"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        error: row.get("error"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
    }
}

pub(crate) struct NewMediaChapterMarker {
    pub(crate) start_position_ticks: i64,
    pub(crate) name: Option<String>,
    pub(crate) marker_type: String,
    pub(crate) chapter_index: i64,
    pub(crate) confidence: f64,
}

#[derive(Debug)]
pub(crate) struct StoredWebSession {
    pub(crate) csrf_token_hash: Vec<u8>,
    pub(crate) device_id: Option<String>,
    pub(crate) user_id: String,
    pub(crate) username_normalized: String,
    pub(crate) display_name: String,
    pub(crate) has_password: bool,
    pub(crate) is_disabled: bool,
    pub(crate) is_admin: bool,
    pub(crate) can_manage_server: bool,
    pub(crate) can_remote_access: bool,
    pub(crate) can_download: bool,
    pub(crate) last_login_at: Option<i64>,
    pub(crate) last_activity_at: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct StoredWebSessionSummary {
    pub(crate) id: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) expires_at: i64,
    pub(crate) last_seen_at: Option<i64>,
    pub(crate) is_current: bool,
}

#[derive(Debug)]
pub(crate) struct StoredDownloadSource {
    pub(crate) source_kind: String,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
}

pub(crate) struct NewAccessToken<'a> {
    pub(crate) id: &'a str,
    pub(crate) token_hash: &'a [u8],
    pub(crate) user_id: &'a str,
    pub(crate) device_id: &'a str,
    pub(crate) client_name: &'a str,
    pub(crate) device_name: &'a str,
    pub(crate) client_version: &'a str,
    pub(crate) device_type: Option<&'a str>,
}

/// A playback-only credential issued for one Web playback session.
pub(crate) struct NewPlaybackAccessToken<'a> {
    pub(crate) id: &'a str,
    pub(crate) token_hash: &'a [u8],
    pub(crate) user_id: &'a str,
    pub(crate) device_id: &'a str,
    pub(crate) client_name: &'a str,
    pub(crate) device_name: &'a str,
    pub(crate) client_version: &'a str,
    pub(crate) playback_session_id: &'a str,
    pub(crate) expires_at: i64,
}

pub(crate) struct NewDevicePairing<'a> {
    pub(crate) id: &'a str,
    pub(crate) user_id: &'a str,
    pub(crate) secret_hash: &'a [u8],
    pub(crate) expires_at: i64,
}

pub(crate) struct NewDeviceAccessToken<'a> {
    pub(crate) id: &'a str,
    pub(crate) token_hash: &'a [u8],
    pub(crate) device_id: &'a str,
    pub(crate) client_name: &'a str,
    pub(crate) device_name: &'a str,
    pub(crate) client_version: &'a str,
    pub(crate) device_type: Option<&'a str>,
}

pub(crate) struct NewLibrary<'a> {
    pub(crate) id: &'a str,
    pub(crate) name: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) realtime_watch_enabled: bool,
    pub(crate) realtime_metadata_auto_match_enabled: bool,
    pub(crate) reconciliation_schedule: Option<&'a str>,
    pub(crate) metadata_schedule: Option<&'a str>,
    pub(crate) scan_concurrency: i64,
    pub(crate) probe_concurrency: i64,
    pub(crate) scraper_id: Option<&'a str>,
    pub(crate) scrapers: &'a [crate::library::LibraryScraper],
    pub(crate) chapter_source_id: Option<&'a str>,
}

pub(crate) struct MediaMetadataUpdate<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) title: &'a str,
    pub(crate) original_title: Option<&'a str>,
    pub(crate) overview: Option<&'a str>,
    pub(crate) production_year: Option<i64>,
    pub(crate) premiere_date: Option<&'a str>,
    pub(crate) rating: Option<f64>,
    pub(crate) rating_source: Option<&'a str>,
    pub(crate) provider_ids_json: Option<&'a str>,
    pub(crate) metadata_fingerprint: &'a [u8],
    pub(crate) provenance_json: &'a str,
    pub(crate) locked_fields_json: &'a str,
}

pub(crate) struct LocalNfoDefaultsRepair<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) provider_ids: &'a BTreeMap<String, String>,
    pub(crate) premiere_date: Option<&'a str>,
}

pub(crate) struct ExternalSubtitleUpdate<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) media_source_id: &'a str,
    pub(crate) stream_index: i64,
    pub(crate) title: Option<&'a str>,
    pub(crate) language: Option<&'a str>,
    pub(crate) is_default: bool,
    pub(crate) is_forced: bool,
}

pub(crate) struct SelectedMetadataUpdate<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) candidate_id: &'a str,
    pub(crate) title: &'a str,
    pub(crate) original_title: Option<&'a str>,
    pub(crate) overview: Option<&'a str>,
    pub(crate) production_year: Option<i64>,
    pub(crate) premiere_date: Option<&'a str>,
    pub(crate) last_air_date: Option<&'a str>,
    pub(crate) status: Option<&'a str>,
    pub(crate) original_language: Option<&'a str>,
    pub(crate) rating: Option<f64>,
    pub(crate) rating_source: Option<&'a str>,
    pub(crate) provider_ids_json: &'a str,
    pub(crate) metadata_scraper_id: Option<&'a str>,
    pub(crate) metadata_fingerprint: &'a [u8],
    pub(crate) provenance_json: &'a str,
    pub(crate) locked_fields_json: &'a str,
    pub(crate) poster_fallback_required: bool,
    pub(crate) keep_pending: bool,
}

pub(crate) struct LibrarySettingsUpdate<'a> {
    pub(crate) name: Option<&'a str>,
    pub(crate) kind: Option<&'a str>,
    pub(crate) is_enabled: Option<bool>,
    pub(crate) realtime_watch_enabled: Option<bool>,
    pub(crate) realtime_metadata_auto_match_enabled: Option<bool>,
    pub(crate) reconciliation_schedule: Option<Option<&'a str>>,
    pub(crate) metadata_schedule: Option<Option<&'a str>>,
    pub(crate) scan_concurrency: Option<i64>,
    pub(crate) probe_concurrency: Option<i64>,
    pub(crate) scraper_id: Option<Option<&'a str>>,
    pub(crate) scrapers: Option<&'a [crate::library::LibraryScraper]>,
    pub(crate) chapter_source_id: Option<Option<&'a str>>,
    pub(crate) media_strategy_json: Option<Option<&'a str>>,
}

pub(crate) struct NewLibraryRoot<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) canonical_path: &'a str,
    pub(crate) display_path: &'a str,
    pub(crate) is_available: bool,
    pub(crate) is_writable: bool,
}

pub(crate) struct NewFilesystemEntry<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_root_id: &'a str,
    pub(crate) relative_path: &'a str,
    pub(crate) entry_kind: &'a str,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: &'a [u8],
    pub(crate) last_seen_generation: &'a str,
}

pub(crate) struct FilesystemEntryUpdate<'a> {
    pub(crate) id: &'a str,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) fingerprint: &'a [u8],
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) last_seen_generation: &'a str,
}

pub(crate) struct FilesystemEntryMove<'a> {
    pub(crate) entry_id: &'a str,
    pub(crate) library_root_id: &'a str,
    pub(crate) relative_path: &'a str,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: &'a [u8],
    pub(crate) generation: &'a str,
}

pub(crate) struct NewMediaItem<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) title: &'a str,
    pub(crate) sort_title: &'a str,
    pub(crate) original_title: Option<&'a str>,
    pub(crate) production_year: Option<i64>,
    pub(crate) provider_ids_json: Option<&'a str>,
}

pub(crate) struct NewHierarchyItem<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) item_type: &'a str,
    pub(crate) parent_id: Option<&'a str>,
    pub(crate) series_id: Option<&'a str>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
    pub(crate) absolute_number: Option<i64>,
    pub(crate) title: &'a str,
    pub(crate) sort_title: &'a str,
    pub(crate) original_title: Option<&'a str>,
    pub(crate) production_year: Option<i64>,
    pub(crate) provider_ids_json: Option<&'a str>,
    pub(crate) identification_status: &'a str,
    pub(crate) identity_key: &'a str,
}

pub(crate) struct NewMediaSource<'a> {
    pub(crate) id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) source_kind: &'a str,
    pub(crate) filesystem_entry_id: &'a str,
    pub(crate) edition_name: Option<&'a str>,
    pub(crate) quality_label: Option<&'a str>,
    pub(crate) container: &'a str,
    pub(crate) size: i64,
    pub(crate) external_url: Option<&'a str>,
    pub(crate) strm_target_kind: Option<&'a str>,
    pub(crate) is_default: bool,
}

#[derive(Clone)]
pub(crate) struct NewMovieFile {
    pub(crate) filesystem_entry_id: String,
    pub(crate) source_id: String,
    pub(crate) relative_path: String,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: Vec<u8>,
    pub(crate) title: String,
    pub(crate) sort_title: String,
    pub(crate) original_title: String,
    pub(crate) production_year: Option<i64>,
    pub(crate) provider_ids_json: Option<String>,
    pub(crate) source_kind: String,
    pub(crate) strm_target_kind: Option<String>,
    pub(crate) edition_name: Option<String>,
    pub(crate) quality_label: Option<String>,
    pub(crate) container: String,
    pub(crate) external_url: Option<String>,
}

#[derive(Clone)]
pub(crate) struct NewEpisodeFile {
    pub(crate) filesystem_entry_id: String,
    pub(crate) source_id: String,
    pub(crate) relative_path: String,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) device: Option<i64>,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: Vec<u8>,
    pub(crate) series_identity: String,
    pub(crate) series_title: String,
    pub(crate) series_sort_title: String,
    pub(crate) series_production_year: Option<i64>,
    pub(crate) series_provider_ids_json: Option<String>,
    pub(crate) season_identity: String,
    pub(crate) season_number: i64,
    pub(crate) episode_identity: String,
    pub(crate) episode_title: String,
    pub(crate) episode_sort_title: String,
    pub(crate) episode_number: i64,
    pub(crate) episode_absolute_number: Option<i64>,
    pub(crate) source_kind: String,
    pub(crate) strm_target_kind: Option<String>,
    pub(crate) edition_name: Option<String>,
    pub(crate) quality_label: Option<String>,
    pub(crate) container: String,
    pub(crate) external_url: Option<String>,
}

pub(crate) struct MediaProbeUpdate<'a> {
    pub(crate) source_id: &'a str,
    pub(crate) container: Option<&'a str>,
    pub(crate) source_size: Option<i64>,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) bitrate: Option<i64>,
    pub(crate) streams: &'a [MediaStreamUpdate<'a>],
    pub(crate) chapters: &'a [MediaInfoChapterUpdate],
}

pub(crate) struct MediaStreamUpdate<'a> {
    pub(crate) stream_index: i64,
    pub(crate) stream_type: &'a str,
    pub(crate) codec: Option<&'a str>,
    pub(crate) language: Option<&'a str>,
    pub(crate) title: Option<&'a str>,
    pub(crate) details_json: Option<&'a str>,
    pub(crate) external_path: Option<&'a str>,
    pub(crate) is_external: bool,
    pub(crate) is_default: bool,
    pub(crate) is_forced: bool,
}

pub(crate) struct MediaInfoChapterUpdate {
    pub(crate) start_position_ticks: i64,
    pub(crate) name: Option<String>,
    pub(crate) chapter_index: i64,
}

#[derive(Debug)]
pub enum StorageError {
    Configuration(DatabaseConfigurationError),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Sqlx {
        path: PathBuf,
        source: sqlx::Error,
    },
    Migration {
        path: PathBuf,
        source: MigrateError,
    },
    Conflict(String),
    Serialization(String),
    LastManager,
    /// The configuration directory belongs to a server that has been set up before, but the
    /// database it points at is missing or empty.
    ///
    /// Refusing here is the difference between a loud failure and a silent one: opening a missing
    /// SQLite file creates it, migrations then build an empty schema, and the next request shows a
    /// setup wizard over data the operator believes still exists.
    EmptyDatabase {
        path: PathBuf,
    },
}

impl StorageError {
    pub(crate) fn is_unique_violation(&self) -> bool {
        matches!(
            self,
            Self::Sqlx { source, .. }
                if source
                    .as_database_error()
                    .is_some_and(|error| error.is_unique_violation())
        )
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(source) => write!(formatter, "数据库配置无效: {source}"),
            Self::Io { path, source } => {
                write!(formatter, "database path '{}': {source}", path.display())
            }
            Self::Sqlx { path, source } => {
                write!(formatter, "database '{}': {source}", path.display())
            }
            Self::Migration { path, source } => {
                write!(
                    formatter,
                    "database migration '{}': {source}",
                    path.display()
                )
            }
            Self::Conflict(source) => write!(formatter, "database conflict: {source}"),
            Self::Serialization(source) => {
                write!(formatter, "database serialization failed: {source}")
            }
            Self::LastManager => {
                formatter.write_str("at least one active server manager is required")
            }
            Self::EmptyDatabase { path } => write!(
                formatter,
                "配置目录已初始化但数据库为空: '{}'。请确认数据卷已正确挂载并恢复该数据库；如果确实要从空库重新开始，删除配置目录中的 server-state.json 后再启动",
                path.display()
            ),
        }
    }
}

/// The SQL text types the `Database::query*` entry points accept.
///
/// Only `&'static str` is memoized: [`ADAPTED_SQL_CACHE`] keys on the address of the text, which
/// stays valid exactly while the text lives in static storage. Strings assembled with `format!`
/// (and the `SqlTemplateCache` templates) keep the previous rewrite-on-every-call behaviour,
/// because their address is reuse of a heap allocation rather than a stable identity.
trait AdaptSqlText {
    /// Hands the text back untouched, with no rewriting and no allocation.
    fn into_sql_str(self) -> sqlx::SqlStr;

    /// Hands back PostgreSQL-ready text: `?` becomes `$1`, `$2`, …
    fn into_postgres_sql(self) -> sqlx::SqlStr;
}

impl AdaptSqlText for &'static str {
    #[inline]
    fn into_sql_str(self) -> sqlx::SqlStr {
        sqlx::SqlStr::from_static(self)
    }

    #[inline]
    fn into_postgres_sql(self) -> sqlx::SqlStr {
        adapt_static_sql_for_backend(self)
    }
}

/// A `SqlStr` of unknown provenance is never memoized: it may own a heap buffer.
impl AdaptSqlText for sqlx::SqlStr {
    #[inline]
    fn into_sql_str(self) -> sqlx::SqlStr {
        self
    }

    #[inline]
    fn into_postgres_sql(self) -> sqlx::SqlStr {
        adapt_postgres_sql_uncached(self)
    }
}

/// `AssertSqlSafe(..)` marks dynamically built SQL, so it always takes the uncached path.
impl<T> AdaptSqlText for sqlx::AssertSqlSafe<T>
where
    sqlx::AssertSqlSafe<T>: sqlx::SqlSafeStr,
{
    #[inline]
    fn into_sql_str(self) -> sqlx::SqlStr {
        sqlx::SqlSafeStr::into_sql_str(self)
    }

    #[inline]
    fn into_postgres_sql(self) -> sqlx::SqlStr {
        adapt_postgres_sql_uncached(self)
    }
}

fn adapt_sql_for_backend(backend: DatabaseBackend, sql: impl AdaptSqlText) -> sqlx::SqlStr {
    if backend == DatabaseBackend::Sqlite {
        // SQLite accepts `?` as-is: the text is never rewritten and never enters the memo.
        return sql.into_sql_str();
    }
    sql.into_postgres_sql()
}

/// PostgreSQL entry point for `&'static str` statements: memoized by statement identity.
fn adapt_static_sql_for_backend(sql: &'static str) -> sqlx::SqlStr {
    adapt_static_sql_for_backend_with_cache(&ADAPTED_SQL_CACHE, sql)
}

/// [`adapt_static_sql_for_backend`] against an explicit cache, so tests can bound and inspect one.
fn adapt_static_sql_for_backend_with_cache(
    cache: &AdaptedSqlCache,
    sql: &'static str,
) -> sqlx::SqlStr {
    let key = StaticSqlKey::of(sql);
    if let Some(adaptation) = cache.lookup(key) {
        return adaptation.into_sql_str(sql);
    }

    let adaptation = match rewrite_postgres_placeholders(sql) {
        None => CachedAdaptation::Unchanged,
        Some(rewritten) => CachedAdaptation::Rewritten(Arc::from(rewritten.as_str())),
    };
    cache.store(key, adaptation.clone());
    adaptation.into_sql_str(sql)
}

/// The pre-cache PostgreSQL rewrite, kept for dynamically built SQL and as the reference the memo
/// is tested against.
fn adapt_postgres_sql_uncached(sql: impl sqlx::SqlSafeStr) -> sqlx::SqlStr {
    let sql = sql.into_sql_str();
    match rewrite_postgres_placeholders(sql.as_str()) {
        None => sql,
        Some(rewritten) => sqlx::SqlSafeStr::into_sql_str(sqlx::AssertSqlSafe(rewritten)),
    }
}

/// Upper bound on memoized PostgreSQL rewrites. A build holds a fixed set of `&'static str`
/// statements, so this is a bounded memo table rather than a cache that grows with traffic.
const MAX_ADAPTED_SQL_CACHE_ENTRIES: usize = 256;
/// Upper bound on the rewritten text the cache keeps alive; `Unchanged` entries cost no text.
const MAX_ADAPTED_SQL_CACHE_TEXT_BYTES: usize = 1024 * 1024;
/// Statements longer than this are rewritten on every call instead of being memoized.
const MAX_ADAPTED_SQL_CACHE_ENTRY_BYTES: usize = 64 * 1024;

/// Identity of a `&'static str` statement. Static storage never moves and is never recycled, so
/// the address plus the length identify the text for the whole process; two distinct static
/// strings cannot share both without sharing the bytes they describe.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct StaticSqlKey {
    address: usize,
    length: usize,
}

impl StaticSqlKey {
    fn of(sql: &'static str) -> Self {
        Self {
            address: sql.as_ptr() as usize,
            length: sql.len(),
        }
    }
}

/// The adaptation of one statement: it either survives verbatim or gains `$n` placeholders.
#[derive(Clone)]
enum CachedAdaptation {
    Unchanged,
    Rewritten(Arc<str>),
}

impl CachedAdaptation {
    fn text_bytes(&self) -> usize {
        match self {
            Self::Unchanged => 0,
            Self::Rewritten(text) => text.len(),
        }
    }

    fn into_sql_str(self, source: &'static str) -> sqlx::SqlStr {
        match self {
            Self::Unchanged => sqlx::SqlStr::from_static(source),
            Self::Rewritten(text) => sqlx::SqlSafeStr::into_sql_str(sqlx::AssertSqlSafe(text)),
        }
    }
}

struct CachedAdaptationEntry {
    adaptation: CachedAdaptation,
    last_used: u64,
}

#[derive(Default)]
struct AdaptedSqlCacheState {
    entries: HashMap<StaticSqlKey, CachedAdaptationEntry>,
    cached_text_bytes: usize,
    clock: u64,
}

/// Bounded least-recently-used memo of PostgreSQL placeholder rewrites for static statements.
///
/// The mutex is only ever held for map operations: `lookup` and `store` never call back into
/// caller code and never await, so the cache cannot deadlock and cannot stall a Tokio worker
/// beyond a hash-map probe.
struct AdaptedSqlCache {
    state: std::sync::Mutex<AdaptedSqlCacheState>,
    max_entries: usize,
    max_text_bytes: usize,
    max_entry_bytes: usize,
}

static ADAPTED_SQL_CACHE: std::sync::LazyLock<AdaptedSqlCache> =
    std::sync::LazyLock::new(AdaptedSqlCache::default);

impl Default for AdaptedSqlCache {
    fn default() -> Self {
        Self::with_limits(
            MAX_ADAPTED_SQL_CACHE_ENTRIES,
            MAX_ADAPTED_SQL_CACHE_TEXT_BYTES,
            MAX_ADAPTED_SQL_CACHE_ENTRY_BYTES,
        )
    }
}

impl AdaptedSqlCache {
    fn with_limits(max_entries: usize, max_text_bytes: usize, max_entry_bytes: usize) -> Self {
        Self {
            state: std::sync::Mutex::new(AdaptedSqlCacheState::default()),
            max_entries,
            max_text_bytes,
            max_entry_bytes,
        }
    }

    /// Returns the memoized adaptation of `key` and refreshes its recency stamp.
    ///
    /// A poisoned lock degrades to a cache miss; rewriting is always correct, so a panic in an
    /// unrelated thread must not turn into a storage failure.
    fn lookup(&self, key: StaticSqlKey) -> Option<CachedAdaptation> {
        let mut state = self.state.lock().ok()?;
        state.clock = state.clock.wrapping_add(1);
        let last_used = state.clock;
        let entry = state.entries.get_mut(&key)?;
        entry.last_used = last_used;
        Some(entry.adaptation.clone())
    }

    fn store(&self, key: StaticSqlKey, adaptation: CachedAdaptation) {
        let text_bytes = adaptation.text_bytes();
        if self.max_entries == 0
            || text_bytes > self.max_entry_bytes
            || text_bytes > self.max_text_bytes
        {
            return;
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.clock = state.clock.wrapping_add(1);
        let last_used = state.clock;
        if let Some(replaced) = state.entries.remove(&key) {
            state.cached_text_bytes = state
                .cached_text_bytes
                .saturating_sub(replaced.adaptation.text_bytes());
        }
        while state.entries.len() >= self.max_entries
            || state.cached_text_bytes.saturating_add(text_bytes) > self.max_text_bytes
        {
            let oldest_key = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key);
            let Some(oldest_key) = oldest_key else {
                break;
            };
            if let Some(removed) = state.entries.remove(&oldest_key) {
                state.cached_text_bytes = state
                    .cached_text_bytes
                    .saturating_sub(removed.adaptation.text_bytes());
            }
        }
        state.cached_text_bytes = state.cached_text_bytes.saturating_add(text_bytes);
        state.entries.insert(
            key,
            CachedAdaptationEntry {
                adaptation,
                last_used,
            },
        );
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.entries.len())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn cached_text_bytes(&self) -> usize {
        self.state
            .lock()
            .map(|state| state.cached_text_bytes)
            .unwrap_or_default()
    }
}

/// Rewrites `?` into `$1`, `$2`, … while copying quoted text and comments verbatim.
///
/// Returns `None` when the statement has no `?` at all, which lets callers keep the source text
/// (and its static or existing allocation) instead of copying it.
fn rewrite_postgres_placeholders(source: &str) -> Option<String> {
    let placeholder_count = source.bytes().filter(|byte| *byte == b'?').count();
    if placeholder_count == 0 {
        return None;
    }

    let placeholder_width = placeholder_count.ilog10() as usize + 1;
    let estimated_growth = placeholder_count.saturating_mul(placeholder_width + 1);
    let mut adapted = String::with_capacity(source.len().saturating_add(estimated_growth));
    let mut parameter_index = 1;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut characters = source.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\'' if !in_double_quote => {
                adapted.push(character);
                if in_single_quote && characters.peek() == Some(&'\'') {
                    if let Some(escaped_quote) = characters.next() {
                        adapted.push(escaped_quote);
                    }
                } else {
                    in_single_quote = !in_single_quote;
                }
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                adapted.push(character);
            }
            // Comments are copied verbatim: a `?` inside one is documentation, not a placeholder.
            '-' if !in_single_quote && !in_double_quote && characters.peek() == Some(&'-') => {
                adapted.push(character);
                if let Some(next) = characters.next() {
                    adapted.push(next);
                }
                for next in characters.by_ref() {
                    adapted.push(next);
                    if next == '\n' {
                        break;
                    }
                }
            }
            '/' if !in_single_quote && !in_double_quote && characters.peek() == Some(&'*') => {
                adapted.push(character);
                if let Some(next) = characters.next() {
                    adapted.push(next);
                }
                let mut previous = '\0';
                for next in characters.by_ref() {
                    adapted.push(next);
                    if previous == '*' && next == '/' {
                        break;
                    }
                    previous = next;
                }
            }
            '?' if !in_single_quote && !in_double_quote => {
                adapted.push('$');
                // Formatting into String cannot fail, but the formatting API returns Result.
                let _ = std::fmt::Write::write_fmt(&mut adapted, format_args!("{parameter_index}"));
                parameter_index += 1;
            }
            _ => adapted.push(character),
        }
    }
    Some(adapted)
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Configuration(source) => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::Sqlx { source, .. } => Some(source),
            Self::Migration { source, .. } => Some(source),
            Self::Conflict(_)
            | Self::LastManager
            | Self::Serialization(_)
            | Self::EmptyDatabase { .. } => None,
        }
    }
}

#[cfg(test)]
mod mail_outbox_tests {
    use super::*;
    use crate::storage::test_database;

    fn new_message<'a>(
        idempotency_key: &'a str,
        recipient_email: &'a str,
        reference_id: &'a str,
        available_at: i64,
    ) -> NewMailOutboxMessage<'a> {
        NewMailOutboxMessage {
            idempotency_key,
            recipient_email,
            template_kind: MailOutboxTemplateKind::Invitation,
            reference_id,
            available_at,
        }
    }

    fn new_challenge<'a>(
        id: &'a str,
        kind: MailChallengeKind,
        recipient_email: &'a str,
        reference_id: &'a str,
        token_hash: &'a [u8],
        expires_at: i64,
    ) -> NewMailChallenge<'a> {
        NewMailChallenge {
            id,
            kind,
            recipient_email,
            reference_id,
            token_hash,
            expires_at,
        }
    }

    #[tokio::test]
    async fn mail_outbox_migration_applies_to_an_empty_sqlite_database()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, database) = test_database().await?;

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_outbox")
            .fetch_one(database.pool())
            .await?;
        assert_eq!(count, 0);
        let columns = sqlx::query("PRAGMA table_info(mail_outbox)")
            .fetch_all(database.pool())
            .await?
            .into_iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        assert!(columns.contains(&"recipient_email".to_owned()));
        assert!(columns.contains(&"template_kind".to_owned()));
        assert!(columns.contains(&"reference_id".to_owned()));
        assert!(!columns.iter().any(|column| {
            matches!(
                column.as_str(),
                "body" | "html_body" | "text_body" | "payload" | "token" | "password"
            )
        }));
        let challenge_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_challenges")
            .fetch_one(database.pool())
            .await?;
        assert_eq!(challenge_count, 0);
        let challenge_columns = sqlx::query("PRAGMA table_info(mail_challenges)")
            .fetch_all(database.pool())
            .await?
            .into_iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        assert_eq!(challenge_columns.len(), 9);
        assert!(challenge_columns.contains(&"token_hash".to_owned()));
        assert!(!challenge_columns.iter().any(|column| {
            matches!(column.as_str(), "token" | "raw_token" | "body" | "password")
        }));
        Ok(())
    }

    #[tokio::test]
    async fn mail_challenge_is_single_use_and_rejects_expired_or_revoked_tokens()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, database) = test_database().await?;
        let invitation_hash = [7_u8; 32];
        let invitation = database
            .create_mail_challenge(
                new_challenge(
                    "challenge-invitation-1",
                    MailChallengeKind::Invitation,
                    "one@example.test",
                    "invite-record-1",
                    &invitation_hash,
                    20,
                ),
                10,
            )
            .await?;
        assert_eq!(invitation.kind, MailChallengeKind::Invitation);
        assert_eq!(invitation.id, "challenge-invitation-1");
        assert_eq!(invitation.token_hash, invitation_hash);
        assert!(database.get_mail_challenge(&invitation.id).await?.is_some());
        assert!(
            database
                .create_mail_challenge(
                    new_challenge(
                        "challenge-invitation-1",
                        MailChallengeKind::Invitation,
                        "one@example.test",
                        "invite-record-1",
                        &invitation_hash,
                        20,
                    ),
                    10,
                )
                .await
                .is_err()
        );
        assert!(
            !database
                .consume_mail_challenge(&invitation.id, &[8_u8; 32], 11)
                .await?
        );
        assert!(
            !database
                .consume_mail_challenge(&invitation.id, &invitation_hash, 20)
                .await?
        );
        assert!(
            database
                .consume_mail_challenge(&invitation.id, &invitation_hash, 19)
                .await?
        );
        assert!(
            !database
                .consume_mail_challenge(&invitation.id, &invitation_hash, 19)
                .await?
        );

        let verification_hash = [9_u8; 32];
        let verification = database
            .create_mail_challenge(
                new_challenge(
                    "challenge-verification-1",
                    MailChallengeKind::EmailVerification,
                    "two@example.test",
                    "registration-2",
                    &verification_hash,
                    30,
                ),
                20,
            )
            .await?;
        assert_eq!(verification.kind, MailChallengeKind::EmailVerification);
        assert_eq!(
            database
                .revoke_mail_challenges(MailChallengeKind::EmailVerification, "registration-2", 21,)
                .await?,
            1
        );
        assert!(
            !database
                .consume_mail_challenge(&verification.id, &verification_hash, 22)
                .await?
        );
        let revoked = database
            .get_mail_challenge(&verification.id)
            .await?
            .ok_or("revoked challenge disappeared")?;
        assert_eq!(revoked.revoked_at, Some(21));
        Ok(())
    }

    #[tokio::test]
    async fn mail_challenge_storage_rejects_invalid_hashes_and_times()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, database) = test_database().await?;
        assert!(matches!(
            database
                .create_mail_challenge(
                    new_challenge(
                        "invalid-hash-challenge",
                        MailChallengeKind::Invitation,
                        "one@example.test",
                        "invite-record-1",
                        &[1_u8; 31],
                        20,
                    ),
                    10,
                )
                .await,
            Err(StorageError::Conflict(_))
        ));
        assert!(matches!(
            database
                .create_mail_challenge(
                    new_challenge(
                        "expired-challenge",
                        MailChallengeKind::Invitation,
                        "one@example.test",
                        "invite-record-1",
                        &[1_u8; 32],
                        10,
                    ),
                    10,
                )
                .await,
            Err(StorageError::Conflict(_))
        ));
        assert!(matches!(
            database
                .consume_mail_challenge("missing", &[1_u8; 31], 11)
                .await,
            Err(StorageError::Conflict(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn mail_challenge_storage_rejects_invalid_ids() -> Result<(), Box<dyn std::error::Error>>
    {
        let (_directory, database) = test_database().await?;
        let long_id = "x".repeat(129);
        database.reset_query_count();
        for id in ["", " padded ", "control\ncharacter", long_id.as_str()] {
            assert!(matches!(
                database
                    .create_mail_challenge(
                        new_challenge(
                            id,
                            MailChallengeKind::Invitation,
                            "one@example.test",
                            "invite-record-1",
                            &[1_u8; 32],
                            20,
                        ),
                        10,
                    )
                    .await,
                Err(StorageError::Conflict(_))
            ));
        }
        assert_eq!(database.query_count(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn mail_outbox_enqueue_is_idempotent_and_rejects_key_reuse()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, database) = test_database().await?;
        let first = database
            .enqueue_mail_outbox(
                new_message("invite:abc", "one@example.test", "invite-abc", 10),
                10,
            )
            .await?;
        let duplicate = database
            .enqueue_mail_outbox(
                new_message("invite:abc", "one@example.test", "invite-abc", 10),
                11,
            )
            .await?;
        assert_eq!(first.id, duplicate.id);
        assert_eq!(duplicate.status, MailOutboxStatus::Pending);

        let conflicting = database
            .enqueue_mail_outbox(
                new_message("invite:abc", "other@example.test", "invite-abc", 10),
                12,
            )
            .await;
        assert!(matches!(conflicting, Err(StorageError::Conflict(_))));
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_outbox")
            .fetch_one(database.pool())
            .await?;
        assert_eq!(total, 1);
        Ok(())
    }

    #[tokio::test]
    async fn mail_outbox_claim_is_atomic_and_recovers_expired_leases()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, database) = test_database().await?;
        let first = database
            .enqueue_mail_outbox(
                new_message("invite:first", "one@example.test", "invite-first", 10),
                10,
            )
            .await?;
        database
            .enqueue_mail_outbox(
                new_message("invite:second", "two@example.test", "invite-second", 10),
                10,
            )
            .await?;

        let (left, right) = tokio::join!(
            database.claim_mail_outbox(10, 5),
            database.claim_mail_outbox(10, 5),
        );
        let left = left?.ok_or("first worker did not claim a message")?;
        let right = right?.ok_or("second worker did not claim a message")?;
        assert_ne!(left.id, right.id);
        assert_eq!(left.attempt_count, 1);
        assert_eq!(left.status, MailOutboxStatus::Sending);
        assert_eq!(left.lease_expires_at, Some(15));

        assert!(database.claim_mail_outbox(14, 5).await?.is_none());
        let reclaimed = database
            .claim_mail_outbox(15, 5)
            .await?
            .ok_or("expired lease was not recovered")?;
        assert_eq!(reclaimed.id, first.id);
        assert_eq!(reclaimed.attempt_count, 2);
        Ok(())
    }

    #[tokio::test]
    async fn mail_outbox_transitions_are_fenced_and_error_codes_are_sanitized()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, database) = test_database().await?;
        let created = database
            .enqueue_mail_outbox(
                new_message("verify:abc", "one@example.test", "challenge-abc", 10),
                10,
            )
            .await?;
        let claimed = database
            .claim_mail_outbox(10, 5)
            .await?
            .ok_or("message was not claimable")?;
        assert!(!database.mark_mail_outbox_sent(&created.id, 9, 11).await?);
        assert!(matches!(
            database
                .mark_mail_outbox_sent(&created.id, claimed.attempt_count, -1)
                .await,
            Err(StorageError::Conflict(_))
        ));
        assert!(matches!(
            database
                .defer_mail_outbox(
                    &created.id,
                    0,
                    20,
                    MailDeliveryErrorCode::SmtpUnavailable,
                    11,
                )
                .await,
            Err(StorageError::Conflict(_))
        ));
        assert!(
            database
                .defer_mail_outbox(
                    &created.id,
                    claimed.attempt_count,
                    20,
                    MailDeliveryErrorCode::SmtpUnavailable,
                    11,
                )
                .await?
        );
        let deferred = database.list_mail_outbox_page(None, 20, 0).await?;
        assert_eq!(deferred.messages[0].status, MailOutboxStatus::RetryWait);
        assert_eq!(
            deferred.messages[0].last_error_code,
            Some(MailDeliveryErrorCode::SmtpUnavailable)
        );

        let retry = database
            .claim_mail_outbox(20, 5)
            .await?
            .ok_or("deferred message was not claimable")?;
        assert_eq!(retry.attempt_count, 2);
        assert!(
            database
                .mark_mail_outbox_sent(&retry.id, retry.attempt_count, 21)
                .await?
        );
        let sent = database.list_mail_outbox_page(None, 20, 0).await?;
        assert_eq!(sent.messages[0].status, MailOutboxStatus::Sent);
        assert_eq!(sent.messages[0].sent_at, Some(21));
        assert_eq!(sent.messages[0].last_error_code, None);
        Ok(())
    }

    #[tokio::test]
    async fn mail_outbox_failure_can_be_manually_retried_and_pages_are_bounded()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_directory, database) = test_database().await?;
        let created = database
            .enqueue_mail_outbox(
                new_message("invite:retry", "one@example.test", "invite-retry", 1),
                1,
            )
            .await?;
        let claimed = database
            .claim_mail_outbox(1, 5)
            .await?
            .ok_or("message was not claimable")?;
        assert!(matches!(
            database
                .fail_mail_outbox(
                    &claimed.id,
                    claimed.attempt_count,
                    MailDeliveryErrorCode::TemplateMissing,
                    -1,
                )
                .await,
            Err(StorageError::Conflict(_))
        ));
        assert!(
            database
                .fail_mail_outbox(
                    &claimed.id,
                    claimed.attempt_count,
                    MailDeliveryErrorCode::TemplateMissing,
                    2,
                )
                .await?
        );
        assert!(matches!(
            database.retry_failed_mail_outbox(&created.id, -1).await,
            Err(StorageError::Conflict(_))
        ));
        assert!(database.retry_failed_mail_outbox(&created.id, 3).await?);
        assert!(!database.retry_failed_mail_outbox(&created.id, 3).await?);

        for index in 0..101 {
            let key = format!("verify:{index}");
            let reference = format!("challenge-{index}");
            database
                .enqueue_mail_outbox(new_message(&key, "one@example.test", &reference, 4), 4)
                .await?;
        }
        let page = database
            .list_mail_outbox_page(Some(MailOutboxStatus::Pending), 1_000, 0)
            .await?;
        assert_eq!(page.total_count, 102);
        assert_eq!(page.messages.len(), 100);
        assert!(
            page.messages
                .iter()
                .all(|message| message.status == MailOutboxStatus::Pending)
        );
        let next_page = database
            .list_mail_outbox_page(Some(MailOutboxStatus::Pending), 100, 100)
            .await?;
        assert_eq!(next_page.messages.len(), 2);
        assert!(
            !page
                .messages
                .iter()
                .any(|message| message.id == next_page.messages[0].id)
        );
        Ok(())
    }
}

#[cfg(test)]
mod storage_error_mapping_tests {
    use super::*;
    use crate::storage::test_database;

    #[tokio::test]
    async fn sqlx_err_maps_failures_exactly_like_storage_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_first_dir, first) = test_database().await?;
        let (_second_dir, second) = test_database().await?;

        let mapped_by_helper = first.sqlx_err()(sqlx::Error::RowNotFound);
        let mapped_by_method = first.storage_error(sqlx::Error::RowNotFound);

        let (
            StorageError::Sqlx {
                path: helper_path,
                source: helper_source,
            },
            StorageError::Sqlx {
                path: method_path,
                source: method_source,
            },
        ) = (&mapped_by_helper, &mapped_by_method)
        else {
            return Err("expected StorageError::Sqlx from both mappings".into());
        };
        assert_eq!(helper_path, method_path);
        assert_eq!(helper_path, &first.path);
        assert!(matches!(helper_source, sqlx::Error::RowNotFound));
        assert!(matches!(method_source, sqlx::Error::RowNotFound));
        assert_eq!(mapped_by_helper.to_string(), mapped_by_method.to_string());
        assert_eq!(
            format!("{mapped_by_helper:?}"),
            format!("{mapped_by_method:?}")
        );

        let other_path = match second.sqlx_err()(sqlx::Error::RowNotFound) {
            StorageError::Sqlx { path, .. } => path,
            _ => return Err("expected StorageError::Sqlx from the second database".into()),
        };
        assert_eq!(&other_path, &second.path);
        assert_ne!(other_path.as_path(), helper_path.as_path());
        Ok(())
    }
}

/// A2: the PostgreSQL placeholder rewrite memo. These tests pin the cached path to the uncached
/// rewrite byte for byte, pin the bounds of the memo table, and cover the SQLite no-op path.
#[cfg(test)]
mod adapted_sql_cache_tests {
    use super::*;

    /// Statements where `?` is not always a placeholder.
    const TRICKY_STATEMENTS: &[&str] = &[
        "SELECT ?, '?' AS literal, \"?\" AS identifier, ?",
        "SELECT * FROM t WHERE a = ? AND b = '?'",
        "SELECT * FROM t -- note: ? placeholder\nWHERE a = ?",
        "SELECT /* a ? b */ * FROM t WHERE a = ?",
        "SELECT 'it''s ?' AS x, ? AS y",
        "SELECT 'a''?''b' AS x, ? AS y",
        "SELECT \"a?\" AS x, ? AS y",
        "?",
        "??",
        "SELECT '?' -- question mark in a line comment\n",
        "SELECT * FROM t WHERE a = ? -- trailing ? comment without newline",
        "SELECT * FROM t /* unterminated ? block comment",
        "SELECT 'unterminated ? literal",
        "SELECT 'no parameters'",
        "SELECT * FROM t -- only a comment mentions ?",
        "SELECT * FROM t /* only a block comment mentions ? */",
        "SELECT * FROM t WHERE a = 'x' || '?'",
        "",
    ];

    fn uncached_rewrite(sql: &'static str) -> String {
        adapt_postgres_sql_uncached(sqlx::AssertSqlSafe(sql.to_owned()))
            .as_str()
            .to_owned()
    }

    fn default_cache() -> AdaptedSqlCache {
        AdaptedSqlCache::with_limits(
            MAX_ADAPTED_SQL_CACHE_ENTRIES,
            MAX_ADAPTED_SQL_CACHE_TEXT_BYTES,
            MAX_ADAPTED_SQL_CACHE_ENTRY_BYTES,
        )
    }

    #[test]
    fn cached_postgres_rewrite_matches_the_uncached_rewrite_for_tricky_statements() {
        let cache = default_cache();
        for sql in TRICKY_STATEMENTS {
            let cached = adapt_static_sql_for_backend_with_cache(&cache, sql);
            assert_eq!(
                cached.as_str(),
                uncached_rewrite(sql),
                "cached rewrite diverged for {sql:?}"
            );
            // A second call must answer from the memo, not from a rebuild.
            let again = adapt_static_sql_for_backend_with_cache(&cache, sql);
            assert_eq!(again.as_str().as_bytes(), cached.as_str().as_bytes());
        }
    }

    #[test]
    fn static_statements_without_question_marks_keep_their_source_pointer() {
        let cache = default_cache();
        let statements = ["SELECT 'no parameters'", "SELECT 1", ""];
        for sql in statements {
            let adapted = adapt_static_sql_for_backend_with_cache(&cache, sql);
            assert_eq!(adapted.as_str(), sql);
            assert!(std::ptr::eq(sql.as_ptr(), adapted.as_str().as_ptr()));
        }
        // These still occupy memo slots, but no rewritten text is retained.
        assert_eq!(cache.entry_count(), statements.len());
        assert_eq!(cache.cached_text_bytes(), 0);
    }

    #[test]
    fn memoized_static_rewrites_reuse_the_cached_buffer() {
        let cache = default_cache();
        let sql = "SELECT /* reuse ? */ ?, ? FROM t";
        let first = adapt_static_sql_for_backend_with_cache(&cache, sql);
        let second = adapt_static_sql_for_backend_with_cache(&cache, sql);
        assert_eq!(first.as_str(), "SELECT /* reuse ? */ $1, $2 FROM t");
        assert_eq!(first.as_str(), second.as_str());
        assert!(std::ptr::eq(
            first.as_str().as_ptr(),
            second.as_str().as_ptr()
        ));
        assert_eq!(cache.entry_count(), 1);
    }

    fn synthetic_statement(seed: u64) -> String {
        const FRAGMENTS: &[&str] = &[
            "SELECT ",
            "FROM t ",
            "WHERE ",
            "a = ?",
            "'?'",
            "'it''s ?'",
            "\"?\"",
            "-- ? comment\n",
            "/* ? */",
            "/* unterminated ?",
            "'unterminated ?",
            "AND b = ?",
            "('LOCAL_FILE', 'STRM_URL')",
            "?",
            " ",
            "\n",
            "c <> 'Who?'",
            "LIMIT ?",
            "ORDER BY id",
            "",
            "??",
        ];
        let mut state = seed | 1;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let fragment_count = usize::try_from(next() % 13).unwrap_or_default();
        let mut sql = String::new();
        for _ in 0..fragment_count {
            let index = usize::try_from(next() % u64::try_from(FRAGMENTS.len()).unwrap_or(1))
                .unwrap_or_default();
            sql.push_str(FRAGMENTS[index]);
        }
        sql
    }

    #[test]
    fn cached_postgres_rewrite_matches_the_uncached_rewrite_for_generated_statements() {
        let cache = default_cache();
        let mut rewritten_count = 0_usize;
        for seed in 1..=2_000_u64 {
            let sql = Box::leak(synthetic_statement(seed).into_boxed_str());
            let cached = adapt_static_sql_for_backend_with_cache(&cache, sql);
            let uncached = uncached_rewrite(sql);
            assert_eq!(
                cached.as_str(),
                uncached,
                "seed {seed} diverged for {sql:?}"
            );
            assert_eq!(cached.as_str().as_bytes(), uncached.as_bytes());
            if cached.as_str().contains('$') {
                rewritten_count += 1;
            }
            assert!(cache.entry_count() <= MAX_ADAPTED_SQL_CACHE_ENTRIES);
            assert!(cache.cached_text_bytes() <= MAX_ADAPTED_SQL_CACHE_TEXT_BYTES);
        }
        assert!(
            rewritten_count > 500,
            "generator produced too few rewritten statements: {rewritten_count}"
        );
    }

    #[test]
    fn static_adaptation_cache_evicts_the_least_recently_used_entry() {
        let cache = AdaptedSqlCache::with_limits(
            2,
            MAX_ADAPTED_SQL_CACHE_TEXT_BYTES,
            MAX_ADAPTED_SQL_CACHE_ENTRY_BYTES,
        );
        let first = "SELECT ? /* lru a */";
        let second = "SELECT ? /* lru b */";
        let third = "SELECT ? /* lru c */";
        for sql in [first, second, first, third] {
            adapt_static_sql_for_backend_with_cache(&cache, sql);
        }
        assert_eq!(cache.entry_count(), 2);
        assert!(cache.lookup(StaticSqlKey::of(first)).is_some());
        assert!(cache.lookup(StaticSqlKey::of(third)).is_some());
        assert!(
            cache.lookup(StaticSqlKey::of(second)).is_none(),
            "the least recently used statement should have been evicted"
        );
    }

    #[test]
    fn static_adaptation_cache_stays_bounded_under_many_distinct_statements() {
        let cache = default_cache();
        for index in 0..(MAX_ADAPTED_SQL_CACHE_ENTRIES * 2) {
            let sql = Box::leak(format!("SELECT ? /* statement {index} */").into_boxed_str());
            let adapted = adapt_static_sql_for_backend_with_cache(&cache, sql);
            assert_eq!(
                adapted.as_str(),
                format!("SELECT $1 /* statement {index} */")
            );
            assert!(cache.entry_count() <= MAX_ADAPTED_SQL_CACHE_ENTRIES);
            assert!(cache.cached_text_bytes() <= MAX_ADAPTED_SQL_CACHE_TEXT_BYTES);
        }
        assert_eq!(cache.entry_count(), MAX_ADAPTED_SQL_CACHE_ENTRIES);
    }

    #[test]
    fn static_adaptation_cache_skips_oversized_statements_and_disabled_caches() {
        let huge_sql = Box::leak(
            format!(
                "SELECT ? /* {} */",
                "x".repeat(MAX_ADAPTED_SQL_CACHE_ENTRY_BYTES)
            )
            .into_boxed_str(),
        );
        let entry_bounded = AdaptedSqlCache::with_limits(
            8,
            MAX_ADAPTED_SQL_CACHE_TEXT_BYTES,
            MAX_ADAPTED_SQL_CACHE_ENTRY_BYTES,
        );
        let adapted = adapt_static_sql_for_backend_with_cache(&entry_bounded, huge_sql);
        assert!(adapted.as_str().starts_with("SELECT $1 /* x"));
        assert_eq!(entry_bounded.entry_count(), 0);

        let text_bounded = AdaptedSqlCache::with_limits(8, 16, MAX_ADAPTED_SQL_CACHE_ENTRY_BYTES);
        let sql = "SELECT ?, ? FROM a_table_that_is_longer_than_the_byte_budget";
        let adapted = adapt_static_sql_for_backend_with_cache(&text_bounded, sql);
        assert_eq!(
            adapted.as_str(),
            "SELECT $1, $2 FROM a_table_that_is_longer_than_the_byte_budget"
        );
        assert_eq!(text_bounded.entry_count(), 0);

        let disabled = AdaptedSqlCache::with_limits(
            0,
            MAX_ADAPTED_SQL_CACHE_TEXT_BYTES,
            MAX_ADAPTED_SQL_CACHE_ENTRY_BYTES,
        );
        let adapted = adapt_static_sql_for_backend_with_cache(&disabled, "SELECT ?");
        assert_eq!(adapted.as_str(), "SELECT $1");
        assert_eq!(disabled.entry_count(), 0);
    }

    #[test]
    fn sqlite_adaptation_returns_the_source_without_rewriting_or_copying() {
        let sql = "SELECT ? /* sqlite keeps placeholders */";
        let adapted = adapt_sql_for_backend(DatabaseBackend::Sqlite, sql);
        assert_eq!(adapted.as_str(), sql);
        assert!(std::ptr::eq(sql.as_ptr(), adapted.as_str().as_ptr()));

        // An owned statement keeps its buffer too, so SQLite rewrites and copies nothing; the memo
        // is only reachable from the PostgreSQL side of `adapt_sql_for_backend`.
        let owned = String::from("SELECT ? FROM t WHERE a = ?");
        let owned_pointer = owned.as_ptr();
        let adapted = adapt_sql_for_backend(DatabaseBackend::Sqlite, sqlx::AssertSqlSafe(owned));
        assert_eq!(adapted.as_str(), "SELECT ? FROM t WHERE a = ?");
        assert!(std::ptr::eq(owned_pointer, adapted.as_str().as_ptr()));

        let shared: std::sync::Arc<str> = std::sync::Arc::from("SELECT ? FROM t");
        let shared_pointer = shared.as_ptr();
        let adapted = adapt_sql_for_backend(DatabaseBackend::Sqlite, sqlx::AssertSqlSafe(shared));
        assert_eq!(adapted.as_str(), "SELECT ? FROM t");
        assert!(std::ptr::eq(shared_pointer, adapted.as_str().as_ptr()));
    }

    #[test]
    fn dynamic_statements_are_rewritten_on_every_call() {
        let sql = "SELECT ?, ? FROM t WHERE a = ?";
        let first = adapt_sql_for_backend(
            DatabaseBackend::Postgres,
            sqlx::AssertSqlSafe(sql.to_owned()),
        );
        let second = adapt_sql_for_backend(
            DatabaseBackend::Postgres,
            sqlx::AssertSqlSafe(sql.to_owned()),
        );
        assert_eq!(first.as_str(), "SELECT $1, $2 FROM t WHERE a = $3");
        assert_eq!(second.as_str(), first.as_str());
        assert!(!std::ptr::eq(
            first.as_str().as_ptr(),
            second.as_str().as_ptr()
        ));

        // A `SqlStr` has unknown provenance, so it is never memoized either.
        let from_str = adapt_sql_for_backend(
            DatabaseBackend::Postgres,
            sqlx::SqlStr::from_static("SELECT ? FROM t"),
        );
        assert_eq!(from_str.as_str(), "SELECT $1 FROM t");
    }

    #[test]
    fn concurrent_static_adaptation_matches_the_uncached_rewrite() {
        let statements: &[&'static str] = &[
            "SELECT ?, '?' AS literal FROM t WHERE a = ?",
            "SELECT * FROM t /* ? */ WHERE b = ?",
            "-- ? comment\nSELECT 1",
            "SELECT 'it''s ?' AS x, ? AS y",
        ];
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..250 {
                        for sql in statements {
                            let cached = adapt_sql_for_backend(DatabaseBackend::Postgres, *sql);
                            assert_eq!(cached.as_str(), uncached_rewrite(sql));
                        }
                    }
                });
            }
        });
        assert!(ADAPTED_SQL_CACHE.entry_count() <= MAX_ADAPTED_SQL_CACHE_ENTRIES);
        assert!(ADAPTED_SQL_CACHE.cached_text_bytes() <= MAX_ADAPTED_SQL_CACHE_TEXT_BYTES);
    }
}

#[cfg(test)]
#[path = "repository_tests.rs"]
mod repository_tests;

#[cfg(all(test, feature = "performance-benchmarks"))]
#[path = "sql_query_benchmarks.rs"]
mod sql_query_benchmarks;

async fn sqlite_has_legacy_timestamp_migration_checksum(
    pool: &AnyPool,
) -> Result<bool, sqlx::Error> {
    let migrations_table_exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = '_sqlx_migrations'
         )",
    )
    .fetch_one(pool)
    .await?;
    if migrations_table_exists == 0 {
        return Ok(false);
    }

    let checksum: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT checksum FROM _sqlx_migrations WHERE version = 126 AND success = 1",
    )
    .fetch_optional(pool)
    .await?;
    Ok(checksum.as_deref() == Some(LEGACY_SQLITE_MIGRATION_0126_CHECKSUM))
}
