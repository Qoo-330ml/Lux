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
    Acquire, Any, AnyPool, Executor, FromRow, Row,
    any::{AnyConnectOptions, AnyPoolOptions},
    migrate::{MigrateError, Migrator},
};
use tokio::{fs, sync::Mutex as AsyncMutex};
use uuid::Uuid;

#[path = "access_changes.rs"]
mod access_changes;
pub(crate) use access_changes::{AccessChangeEpochs, AccessEpoch};
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
    access_changes: Arc<AccessChangeEpochs>,
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

/// Row of the persistent recommendation rating-median cache read.
#[derive(Debug, sqlx::FromRow)]
struct RecommendationRatingMedianRow {
    median_rating: f64,
    calculated_at: i64,
}

/// Row of the `_sqlx_migrations` schema version projection.
#[derive(Debug, sqlx::FromRow)]
struct SchemaVersionRow {
    version: i64,
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
            access_changes: Arc::new(AccessChangeEpochs::default()),
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
            let row = RecommendationRatingMedianRow::from_row(&row).map_err(self.sqlx_err())?;
            if recommendation_rating_cache_is_fresh(now, row.calculated_at) {
                let median = row.median_rating;
                self.recommendation_rating_median_cache
                    .lock()
                    .await
                    .values
                    .insert(
                        cache_key,
                        RecommendationRatingMedianEntry {
                            value: median,
                            calculated_at: row.calculated_at,
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
        let row = self
            .query("SELECT COALESCE(MAX(version), 0) AS version FROM _sqlx_migrations")
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let row = SchemaVersionRow::from_row(&row).map_err(self.sqlx_err())?;
        Ok(row.version)
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

    /// The change counters that the playback access cache stamps its entries with.
    ///
    /// The permission writes in this module bump these counters; the cache only reads them.
    pub(crate) fn access_changes(&self) -> &AccessChangeEpochs {
        &self.access_changes
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

/// Row of every user projection that decodes into [`StoredUser`].
///
/// The permission flags stay `i64` because both backends store them as `BIGINT` 0/1 and the
/// conversion to `bool` happens in [`stored_user`].
#[derive(Debug, sqlx::FromRow)]
struct StoredUserRow {
    id: String,
    username_normalized: String,
    display_name: String,
    password_hash: String,
    has_password: i64,
    is_disabled: i64,
    is_admin: i64,
    can_manage_server: i64,
    can_remote_access: i64,
    can_download: i64,
    email: Option<String>,
    email_verified_at: Option<i64>,
    deleted_at: Option<i64>,
    deletion_purge_at: Option<i64>,
    last_login_at: Option<i64>,
    last_activity_at: Option<i64>,
}

pub(crate) fn stored_user(row: sqlx::any::AnyRow) -> Result<StoredUser, sqlx::Error> {
    let row = StoredUserRow::from_row(&row)?;
    Ok(StoredUser {
        id: row.id,
        username_normalized: row.username_normalized,
        display_name: row.display_name,
        password_hash: row.password_hash,
        has_password: row.has_password != 0,
        is_disabled: row.is_disabled != 0,
        is_admin: row.is_admin != 0,
        can_manage_server: row.can_manage_server != 0,
        can_remote_access: row.can_remote_access != 0,
        can_download: row.can_download != 0,
        email: row.email,
        email_verified_at: row.email_verified_at,
        deleted_at: row.deleted_at,
        deletion_purge_at: row.deletion_purge_at,
        last_login_at: row.last_login_at,
        last_activity_at: row.last_activity_at,
    })
}

/// Row of the administrator user list projection that decodes into
/// [`StoredAdminUserListItem`].
#[derive(Debug, sqlx::FromRow)]
struct StoredAdminUserListItemRow {
    id: String,
    username_normalized: String,
    display_name: String,
    has_password: i64,
    is_disabled: i64,
    is_admin: i64,
    can_manage_server: i64,
    can_remote_access: i64,
    can_download: i64,
    email: Option<String>,
    email_verified_at: Option<i64>,
    deleted_at: Option<i64>,
    deletion_purge_at: Option<i64>,
    last_login_at: Option<i64>,
    last_activity_at: Option<i64>,
    created_at: i64,
    current_playback_count: i64,
}

pub(crate) fn stored_admin_user_list_item(
    row: sqlx::any::AnyRow,
) -> Result<StoredAdminUserListItem, sqlx::Error> {
    let row = StoredAdminUserListItemRow::from_row(&row)?;
    Ok(StoredAdminUserListItem {
        id: row.id,
        username_normalized: row.username_normalized,
        display_name: row.display_name,
        has_password: row.has_password != 0,
        is_disabled: row.is_disabled != 0,
        is_admin: row.is_admin != 0,
        can_manage_server: row.can_manage_server != 0,
        can_remote_access: row.can_remote_access != 0,
        can_download: row.can_download != 0,
        email: row.email,
        email_verified_at: row.email_verified_at,
        deleted_at: row.deleted_at,
        deletion_purge_at: row.deletion_purge_at,
        last_login_at: row.last_login_at,
        last_activity_at: row.last_activity_at,
        created_at: row.created_at,
        current_playback_count: row.current_playback_count,
    })
}

/// Row of the administrator permission-preset source projection.
#[derive(Debug, sqlx::FromRow)]
struct StoredUserPermissionPresetSourceRow {
    preset_id: String,
    preset_name: String,
    preset_version: i64,
    applied_at: i64,
}

// LUX-648 uses this mapping when building administrator user projections.
#[allow(dead_code)]
pub(crate) fn stored_user_permission_preset_source(
    row: sqlx::any::AnyRow,
) -> Result<StoredUserPermissionPresetSource, sqlx::Error> {
    let row = StoredUserPermissionPresetSourceRow::from_row(&row)?;
    Ok(StoredUserPermissionPresetSource {
        preset_id: row.preset_id,
        preset_name: row.preset_name,
        preset_version: row.preset_version,
        applied_at: row.applied_at,
    })
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

/// Row of the scheduled task config projections that decode into
/// [`StoredScheduledTaskConfig`].
///
/// The library name is a nullable column because the query joins the owner library with a
/// `LEFT JOIN` to keep `GLOBAL` owners decodable.
#[derive(Debug, sqlx::FromRow)]
struct StoredScheduledTaskConfigRow {
    owner_type: String,
    owner_id: String,
    task_type: String,
    plan_id: Option<String>,
    task_name: String,
    task_description: String,
    source_type: String,
    plugin_id: Option<String>,
    cron_or_interval: Option<String>,
    is_enabled: i64,
    resource_limit_json: String,
    created_at: i64,
    updated_at: i64,
    library_name: Option<String>,
}

fn stored_scheduled_task(row: sqlx::any::AnyRow) -> Result<StoredScheduledTaskConfig, sqlx::Error> {
    let row = StoredScheduledTaskConfigRow::from_row(&row)?;
    Ok(StoredScheduledTaskConfig {
        owner_type: row.owner_type,
        owner_id: row.owner_id,
        task_type: row.task_type,
        plan_id: row.plan_id,
        task_name: row.task_name,
        task_description: row.task_description,
        source_type: row.source_type,
        plugin_id: row.plugin_id,
        cron_or_interval: row.cron_or_interval,
        is_enabled: row.is_enabled != 0,
        resource_limit_json: row.resource_limit_json,
        created_at: row.created_at,
        updated_at: row.updated_at,
        library_name: row.library_name,
    })
}

/// Row of the scheduled task plan projection. The attached libraries are loaded separately by
/// `load_scheduled_task_plan_libraries`, so they are not part of this row.
#[derive(Debug, sqlx::FromRow)]
struct StoredScheduledTaskPlanRow {
    id: String,
    task_type: String,
    plan_name: String,
    task_name: String,
    task_description: String,
    source_type: String,
    plugin_id: Option<String>,
    cron_or_interval: Option<String>,
    is_enabled: i64,
    resource_limit_json: String,
    scope_type: String,
    is_default: i64,
    created_at: i64,
    updated_at: i64,
}

fn stored_scheduled_task_plan(
    row: sqlx::any::AnyRow,
) -> Result<StoredScheduledTaskPlan, sqlx::Error> {
    let row = StoredScheduledTaskPlanRow::from_row(&row)?;
    Ok(StoredScheduledTaskPlan {
        id: row.id,
        task_type: row.task_type,
        plan_name: row.plan_name,
        task_name: row.task_name,
        task_description: row.task_description,
        source_type: row.source_type,
        plugin_id: row.plugin_id,
        cron_or_interval: row.cron_or_interval,
        is_enabled: row.is_enabled != 0,
        resource_limit_json: row.resource_limit_json,
        scope_type: row.scope_type,
        is_default: row.is_default != 0,
        created_at: row.created_at,
        updated_at: row.updated_at,
        libraries: Vec::new(),
    })
}

/// Row of the notification destination projections.
#[derive(Debug, sqlx::FromRow)]
struct StoredNotificationDestinationRow {
    id: String,
    name: String,
    url: String,
    enabled: i64,
    allow_private_network: i64,
    event_types_json: String,
    payload_format: String,
    provider_plugin_id: String,
    provider_config_json: String,
    created_at: i64,
    updated_at: i64,
}

fn stored_notification_destination(
    row: sqlx::any::AnyRow,
) -> Result<StoredNotificationDestination, sqlx::Error> {
    let row = StoredNotificationDestinationRow::from_row(&row)?;
    Ok(StoredNotificationDestination {
        id: row.id,
        name: row.name,
        url: row.url,
        enabled: row.enabled != 0,
        allow_private_network: row.allow_private_network != 0,
        event_types_json: row.event_types_json,
        payload_format: row.payload_format,
        provider_plugin_id: row.provider_plugin_id,
        provider_config_json: row.provider_config_json,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// Row of the notification delivery projections.
///
/// The destination is joined in, so its `name`/`url` columns decode into the delivery's
/// `destination_name`/`destination_url` fields.
#[derive(Debug, sqlx::FromRow)]
struct StoredNotificationDeliveryRow {
    id: String,
    event_id: String,
    destination_id: String,
    status: String,
    attempt_count: i64,
    next_attempt_at: i64,
    last_http_status: Option<i64>,
    last_error: Option<String>,
    delivered_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
    event_type: String,
    payload_json: String,
    name: String,
    url: String,
    allow_private_network: i64,
    provider_plugin_id: String,
    provider_config_json: String,
}

fn stored_notification_delivery(
    row: sqlx::any::AnyRow,
) -> Result<StoredNotificationDelivery, sqlx::Error> {
    let row = StoredNotificationDeliveryRow::from_row(&row)?;
    Ok(StoredNotificationDelivery {
        id: row.id,
        event_id: row.event_id,
        destination_id: row.destination_id,
        status: row.status,
        attempt_count: row.attempt_count,
        next_attempt_at: row.next_attempt_at,
        last_http_status: row.last_http_status,
        last_error: row.last_error,
        delivered_at: row.delivered_at,
        created_at: row.created_at,
        updated_at: row.updated_at,
        event_type: row.event_type,
        payload_json: row.payload_json,
        destination_name: row.name,
        destination_url: row.url,
        allow_private_network: row.allow_private_network != 0,
        provider_plugin_id: row.provider_plugin_id,
        provider_config_json: row.provider_config_json,
    })
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

/// Row of the scan-job projections that decode into [`StoredScanJob`].
///
/// The three flag columns stay `i64` because both backends store them as `BIGINT` 0/1; the
/// conversion to `bool` happens in [`stored_scan_job`].
#[derive(Debug, sqlx::FromRow)]
struct StoredScanJobRow {
    id: String,
    library_id: String,
    job_type: String,
    status: String,
    generation: String,
    cursor: Option<String>,
    processed_count: i64,
    total_count: i64,
    processed_media_count: Option<i64>,
    cancel_requested: i64,
    error: Option<String>,
    created_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    discovery_completed: i64,
    auto_metadata_match: i64,
    current_item: Option<String>,
    scan_phase: String,
}

fn stored_scan_job(row: sqlx::any::AnyRow) -> Result<StoredScanJob, sqlx::Error> {
    let row = StoredScanJobRow::from_row(&row)?;
    Ok(StoredScanJob {
        id: row.id,
        library_id: row.library_id,
        job_type: row.job_type,
        status: row.status,
        generation: row.generation,
        cursor: row.cursor,
        processed_count: row.processed_count,
        total_count: row.total_count,
        processed_media_count: row.processed_media_count,
        cancel_requested: row.cancel_requested != 0,
        error: row.error,
        created_at: row.created_at,
        started_at: row.started_at,
        finished_at: row.finished_at,
        discovery_completed: row.discovery_completed != 0,
        auto_metadata_match: row.auto_metadata_match != 0,
        current_item: row.current_item,
        scan_phase: row.scan_phase,
    })
}

/// Row of the local-metadata batch projections that decode into
/// [`StoredScanLocalMetadataBatch`].
#[derive(Debug, sqlx::FromRow)]
struct StoredScanLocalMetadataBatchRow {
    id: String,
    job_id: String,
    library_root_id: String,
    batch_sequence: i64,
    source_refs_json: String,
    non_retryable_item_ids_json: String,
    source_count: i64,
    images_completed_at: Option<i64>,
    status: String,
    attempts: i64,
    next_attempt_at: Option<i64>,
    error: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn stored_scan_local_metadata_batch(
    row: sqlx::any::AnyRow,
) -> Result<StoredScanLocalMetadataBatch, sqlx::Error> {
    let row = StoredScanLocalMetadataBatchRow::from_row(&row)?;
    Ok(StoredScanLocalMetadataBatch {
        id: row.id,
        job_id: row.job_id,
        library_root_id: row.library_root_id,
        batch_sequence: row.batch_sequence,
        source_refs_json: row.source_refs_json,
        non_retryable_item_ids_json: row.non_retryable_item_ids_json,
        source_count: row.source_count,
        images_completed_at: row.images_completed_at,
        status: row.status,
        attempts: row.attempts,
        next_attempt_at: row.next_attempt_at,
        error: row.error,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// Row of the pending scan-job path projections that decode into [`StoredScanJobPath`].
///
/// `file_change_revision` has no column: normal scan paths never carry a revision, so the
/// decoder keeps defaulting it to `None`.
#[derive(Debug, sqlx::FromRow)]
struct StoredScanJobPathRow {
    library_root_id: String,
    relative_path: String,
    change_kind: String,
}

fn stored_scan_job_path(row: sqlx::any::AnyRow) -> Result<StoredScanJobPath, sqlx::Error> {
    let row = StoredScanJobPathRow::from_row(&row)?;
    Ok(StoredScanJobPath {
        library_root_id: row.library_root_id,
        relative_path: row.relative_path,
        change_kind: row.change_kind,
        file_change_revision: None,
    })
}

/// Row of the pending reconciliation-entry projections that decode into
/// [`StoredReconciliationScanEntry`].
#[derive(Debug, sqlx::FromRow)]
struct StoredReconciliationScanEntryRow {
    library_root_id: String,
    relative_path: String,
}

fn stored_reconciliation_scan_entry(
    row: sqlx::any::AnyRow,
) -> Result<StoredReconciliationScanEntry, sqlx::Error> {
    let row = StoredReconciliationScanEntryRow::from_row(&row)?;
    Ok(StoredReconciliationScanEntry {
        library_root_id: row.library_root_id,
        relative_path: row.relative_path,
    })
}

/// Row of the STRM probe job projections that decode into [`StoredStrmProbeJob`].
///
/// The flag columns stay `i64` because both backends store them as `BIGINT` 0/1; the
/// conversion to `bool` happens in [`stored_strm_probe_job`].
#[derive(Debug, sqlx::FromRow)]
struct StoredStrmProbeJobRow {
    id: String,
    operation_id: String,
    library_id: String,
    status: String,
    concurrency: i64,
    include_ready: i64,
    write_sidecars: i64,
    media_info_enabled: i64,
    thumbnail_enabled: i64,
    thumbnail_position_percent: i64,
    target_scan_job_id: Option<String>,
    target_source_id: Option<String>,
    cursor: Option<String>,
    processed_count: i64,
    total_count: i64,
    cancel_requested: i64,
    error: Option<String>,
    created_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
}

fn stored_strm_probe_job(row: sqlx::any::AnyRow) -> Result<StoredStrmProbeJob, sqlx::Error> {
    let row = StoredStrmProbeJobRow::from_row(&row)?;
    Ok(StoredStrmProbeJob {
        id: row.id,
        operation_id: row.operation_id,
        library_id: row.library_id,
        status: row.status,
        concurrency: row.concurrency,
        include_ready: row.include_ready != 0,
        write_sidecars: row.write_sidecars != 0,
        media_info_enabled: row.media_info_enabled != 0,
        thumbnail_enabled: row.thumbnail_enabled != 0,
        thumbnail_position_percent: row.thumbnail_position_percent,
        target_scan_job_id: row.target_scan_job_id,
        target_source_id: row.target_source_id,
        cursor: row.cursor,
        processed_count: row.processed_count,
        total_count: row.total_count,
        cancel_requested: row.cancel_requested != 0,
        error: row.error,
        created_at: row.created_at,
        started_at: row.started_at,
        finished_at: row.finished_at,
    })
}

/// Row of the chapter-detection job projections that decode into
/// [`StoredChapterDetectionJob`].
///
/// `cancel_requested` stays `i64` because both backends store it as `BIGINT` 0/1; the
/// conversion to `bool` happens in [`stored_chapter_detection_job`].
#[derive(Debug, sqlx::FromRow)]
struct StoredChapterDetectionJobRow {
    id: String,
    library_id: String,
    plugin_id: String,
    status: String,
    concurrency: i64,
    intro_window_seconds: i64,
    credits_window_seconds: i64,
    match_threshold: f64,
    cursor: Option<String>,
    processed_count: i64,
    total_count: i64,
    cancel_requested: i64,
    error: Option<String>,
    created_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
}

fn stored_chapter_detection_job(
    row: sqlx::any::AnyRow,
) -> Result<StoredChapterDetectionJob, sqlx::Error> {
    let row = StoredChapterDetectionJobRow::from_row(&row)?;
    Ok(StoredChapterDetectionJob {
        id: row.id,
        library_id: row.library_id,
        plugin_id: row.plugin_id,
        status: row.status,
        concurrency: row.concurrency,
        intro_window_seconds: row.intro_window_seconds,
        credits_window_seconds: row.credits_window_seconds,
        match_threshold: row.match_threshold,
        cursor: row.cursor,
        processed_count: row.processed_count,
        total_count: row.total_count,
        cancel_requested: row.cancel_requested != 0,
        error: row.error,
        created_at: row.created_at,
        started_at: row.started_at,
        finished_at: row.finished_at,
    })
}

/// Row of the library-root projections that decode into [`StoredLibraryRoot`].
///
/// The availability flags stay `i64` because both backends store them as `BIGINT` 0/1; the
/// conversion to `bool` happens in [`stored_library_root`].
#[derive(Debug, sqlx::FromRow)]
struct StoredLibraryRootRow {
    id: String,
    library_id: String,
    canonical_path: String,
    display_path: String,
    is_available: i64,
    is_writable: i64,
    last_checked_at: i64,
    unavailable_since: Option<i64>,
    scan_cursor: Option<String>,
}

fn stored_library_root(row: sqlx::any::AnyRow) -> Result<StoredLibraryRoot, sqlx::Error> {
    let row = StoredLibraryRootRow::from_row(&row)?;
    Ok(StoredLibraryRoot {
        id: row.id,
        library_id: row.library_id,
        canonical_path: row.canonical_path,
        display_path: row.display_path,
        is_available: row.is_available != 0,
        is_writable: row.is_writable != 0,
        last_checked_at: row.last_checked_at,
        unavailable_since: row.unavailable_since,
        scan_cursor: row.scan_cursor,
    })
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

/// Row of the filesystem-entry projections that decode into [`StoredFilesystemEntry`].
///
/// The item/parent/series columns come from `LEFT JOIN`s, so they are nullable; `is_missing`
/// stays `i64` because both backends store it as `BIGINT` 0/1.
#[derive(Debug, sqlx::FromRow)]
struct StoredFilesystemEntryRow {
    id: String,
    relative_path: String,
    entry_kind: String,
    size: i64,
    modified_at: i64,
    device: Option<i64>,
    inode: Option<i64>,
    fingerprint: Option<Vec<u8>>,
    last_seen_generation: String,
    is_missing: i64,
    item_id: Option<String>,
    parent_identity_key: Option<String>,
    item_type: Option<String>,
    item_identity_key: Option<String>,
    series_provider_ids_json: Option<String>,
}

fn stored_filesystem_entry(row: sqlx::any::AnyRow) -> Result<StoredFilesystemEntry, sqlx::Error> {
    let row = StoredFilesystemEntryRow::from_row(&row)?;
    Ok(StoredFilesystemEntry {
        id: row.id,
        relative_path: row.relative_path,
        entry_kind: row.entry_kind,
        size: row.size,
        modified_at: row.modified_at,
        device: row.device,
        inode: row.inode,
        fingerprint: row.fingerprint,
        last_seen_generation: row.last_seen_generation,
        is_missing: row.is_missing != 0,
        item_id: row.item_id,
        parent_identity_key: row.parent_identity_key,
        item_type: row.item_type,
        item_identity_key: row.item_identity_key,
        series_provider_ids_json: row.series_provider_ids_json,
    })
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

/// Row of the canonical-person projections that decode into [`StoredCanonicalPerson`].
#[derive(Debug, sqlx::FromRow)]
struct StoredCanonicalPersonRow {
    id: String,
}

fn stored_canonical_person(row: sqlx::any::AnyRow) -> Result<StoredCanonicalPerson, sqlx::Error> {
    let row = StoredCanonicalPersonRow::from_row(&row)?;
    Ok(StoredCanonicalPerson { id: row.id })
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

/// Row of the person-match candidate projections that decode into
/// [`StoredPersonMatchCandidate`].
#[derive(Debug, sqlx::FromRow)]
struct StoredPersonMatchCandidateRow {
    id: String,
    item_id: String,
    provider: String,
    provider_id: String,
    candidate_person_ids_json: String,
    status: String,
    score: Option<f64>,
    evidence_json: String,
    target_person_id: Option<String>,
    previous_person_id: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn stored_person_match_candidate(
    row: sqlx::any::AnyRow,
) -> Result<StoredPersonMatchCandidate, sqlx::Error> {
    let row = StoredPersonMatchCandidateRow::from_row(&row)?;
    Ok(StoredPersonMatchCandidate {
        id: row.id,
        item_id: row.item_id,
        provider: row.provider,
        provider_id: row.provider_id,
        candidate_person_ids_json: row.candidate_person_ids_json,
        status: row.status,
        score: row.score,
        evidence_json: row.evidence_json,
        target_person_id: row.target_person_id,
        previous_person_id: row.previous_person_id,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
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

/// Row of the person-credit projections that decode into [`StoredPersonCredit`].
///
/// `lux_person_id` keeps `#[sqlx(default)]` because the former decoder used
/// `try_get(..).ok()`: a projection that omits the column still decodes to `None` instead of
/// failing the whole row. The JSON columns are decoded by [`person_credit_json`].
#[derive(Debug, sqlx::FromRow)]
struct StoredPersonCreditRow {
    item_id: String,
    person_id: String,
    #[sqlx(default)]
    lux_person_id: Option<String>,
    provider: String,
    person_name: String,
    role: String,
    date_created: i64,
    biography: Option<String>,
    birthday: Option<String>,
    deathday: Option<String>,
    known_for_department: Option<String>,
    place_of_birth: Option<String>,
    provider_ids_json: String,
    genres_json: String,
    tags_json: String,
    production_locations_json: String,
    premiere_date: Option<String>,
    production_year: Option<i64>,
    taglines_json: String,
}

fn stored_person_credit(row: sqlx::any::AnyRow) -> Result<StoredPersonCredit, sqlx::Error> {
    let row = StoredPersonCreditRow::from_row(&row)?;
    Ok(StoredPersonCredit {
        item_id: row.item_id,
        person_id: row.person_id,
        lux_person_id: row.lux_person_id,
        provider: row.provider,
        person_name: row.person_name,
        role: row.role,
        date_created: row.date_created,
        biography: row.biography,
        birthday: row.birthday,
        deathday: row.deathday,
        known_for_department: row.known_for_department,
        place_of_birth: row.place_of_birth,
        provider_ids: person_credit_json(&row.provider_ids_json, "provider_ids_json"),
        genres: person_credit_json(&row.genres_json, "genres_json"),
        tags: person_credit_json(&row.tags_json, "tags_json"),
        production_locations: person_credit_json(
            &row.production_locations_json,
            "production_locations_json",
        ),
        premiere_date: row.premiere_date,
        production_year: row.production_year,
        taglines: person_credit_json(&row.taglines_json, "taglines_json"),
    })
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

/// Row of the person-index rebuild job projections that decode into
/// [`StoredPersonIndexRebuildJob`].
///
/// `cancel_requested` stays `i64` because both backends store it as `BIGINT` 0/1.
#[derive(Debug, sqlx::FromRow)]
struct StoredPersonIndexRebuildJobRow {
    library_id: String,
    status: String,
    cursor_id: Option<String>,
    processed_count: i64,
    total_count: i64,
    cancel_requested: i64,
}

fn stored_person_index_rebuild_job(
    row: sqlx::any::AnyRow,
) -> Result<StoredPersonIndexRebuildJob, sqlx::Error> {
    let row = StoredPersonIndexRebuildJobRow::from_row(&row)?;
    Ok(StoredPersonIndexRebuildJob {
        library_id: row.library_id,
        status: row.status,
        cursor_id: row.cursor_id,
        processed_count: row.processed_count,
        total_count: row.total_count,
        cancel_requested: row.cancel_requested != 0,
    })
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

/// Row of every [`StoredMetadataReidentifyJob`] projection.
///
/// `cancel_requested` stays `i64` because both backends store the flag as `BIGINT` 0/1 and the
/// conversion to `bool` happens in [`stored_metadata_reidentify_job`].
#[derive(Debug, sqlx::FromRow)]
struct StoredMetadataReidentifyJobRow {
    id: String,
    status: String,
    processed_count: i64,
    total_count: i64,
    error: Option<String>,
    created_at: i64,
    updated_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    mode: String,
    cancel_requested: i64,
    library_id: Option<String>,
    job_scope: String,
    pending_count: i64,
}

fn stored_metadata_reidentify_job(
    row: sqlx::any::AnyRow,
) -> Result<StoredMetadataReidentifyJob, sqlx::Error> {
    let row = StoredMetadataReidentifyJobRow::from_row(&row)?;
    Ok(StoredMetadataReidentifyJob {
        id: row.id,
        status: row.status,
        processed_count: row.processed_count,
        total_count: row.total_count,
        error: row.error,
        created_at: row.created_at,
        updated_at: row.updated_at,
        started_at: row.started_at,
        finished_at: row.finished_at,
        mode: row.mode,
        cancel_requested: row.cancel_requested != 0,
        library_id: row.library_id,
        job_scope: row.job_scope,
        pending_count: row.pending_count,
    })
}

/// Row of every [`StoredMetadataReidentifyItem`] projection.
#[derive(Debug, sqlx::FromRow)]
struct StoredMetadataReidentifyItemRow {
    job_id: String,
    item_id: String,
    status: String,
    candidate_count: i64,
    error: Option<String>,
    updated_at: i64,
}

fn stored_metadata_reidentify_item(
    row: sqlx::any::AnyRow,
) -> Result<StoredMetadataReidentifyItem, sqlx::Error> {
    let row = StoredMetadataReidentifyItemRow::from_row(&row)?;
    Ok(StoredMetadataReidentifyItem {
        job_id: row.job_id,
        item_id: row.item_id,
        status: row.status,
        candidate_count: row.candidate_count,
        error: row.error,
        updated_at: row.updated_at,
    })
}

/// Row of every [`StoredMetadataCandidate`] projection; `item_title` comes from the joined
/// `media_items.title`.
#[derive(Debug, sqlx::FromRow)]
struct StoredMetadataCandidateRow {
    id: String,
    item_id: String,
    provider: String,
    provider_id: String,
    candidate_json: String,
    score: f64,
    status: String,
    expires_at: Option<i64>,
    item_title: String,
}

fn stored_metadata_candidate(
    row: sqlx::any::AnyRow,
) -> Result<StoredMetadataCandidate, sqlx::Error> {
    let row = StoredMetadataCandidateRow::from_row(&row)?;
    Ok(StoredMetadataCandidate {
        id: row.id,
        item_id: row.item_id,
        provider: row.provider,
        provider_id: row.provider_id,
        candidate_json: row.candidate_json,
        score: row.score,
        status: row.status,
        expires_at: row.expires_at,
        item_title: row.item_title,
    })
}

/// Row of every [`StoredItemMetadataCompleteness`] projection.
///
/// `is_missing` stays `Option<i64>` because a non-`READY` row stores `NULL` and the conversion to
/// `Option<bool>` happens in [`stored_item_metadata_completeness`].
#[derive(Debug, sqlx::FromRow)]
struct StoredItemMetadataCompletenessRow {
    item_id: String,
    capability: String,
    local_state: String,
    is_missing: Option<i64>,
    input_fingerprint: Option<Vec<u8>>,
    checked_at: Option<i64>,
    retry_after: Option<i64>,
    error: Option<String>,
    updated_at: i64,
}

fn stored_item_metadata_completeness(
    row: sqlx::any::AnyRow,
) -> Result<StoredItemMetadataCompleteness, sqlx::Error> {
    let row = StoredItemMetadataCompletenessRow::from_row(&row)?;
    Ok(StoredItemMetadataCompleteness {
        item_id: row.item_id,
        capability: row.capability,
        local_state: row.local_state,
        is_missing: row.is_missing.map(|value| value != 0),
        input_fingerprint: row.input_fingerprint,
        checked_at: row.checked_at,
        retry_after: row.retry_after,
        error: row.error,
        updated_at: row.updated_at,
    })
}

/// Row of every [`StoredMediaItem`] projection.
#[derive(Debug, sqlx::FromRow)]
struct StoredMediaItemRow {
    id: String,
}

fn stored_media_item(row: sqlx::any::AnyRow) -> Result<StoredMediaItem, sqlx::Error> {
    let row = StoredMediaItemRow::from_row(&row)?;
    Ok(StoredMediaItem { id: row.id })
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
    /// State the same session reported before this event, when it already existed. A matching
    /// state means the event is a plain progress tick, which must not rewrite the derived resume
    /// pointer or the played container states.
    pub(crate) previous_state: Option<&'a str>,
    /// Position the same session reported before this event, when it already existed. A tick that
    /// moves the position across the configured resume threshold adds the item to, or removes it
    /// from, continue-watching, so that tick refreshes the derived pointer like a state change.
    pub(crate) previous_position_ticks: Option<i64>,
    /// Renew the matching playback admission in the same transaction as the session and item
    /// state writes. Callers that renew on a separate statement leave this `false` so the write
    /// pattern stays unchanged for their paths.
    pub(crate) renew_admission: bool,
}

/// Episode container identifiers, read once while a playback event refreshes the resume pointer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlaybackContainerScope {
    pub(crate) parent_id: Option<String>,
    pub(crate) series_id: Option<String>,
}

/// What [`Database::record_playback_event`] changed beyond the reported progress position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PlaybackEventEffects {
    /// `user_item_state.is_played` flipped from `0` to `1` while recording this event.
    pub(crate) is_played_changed: bool,
    /// Episode scope read while refreshing the resume pointer. It is present whenever the event
    /// ran that path for an episode, even when the episode has no series and therefore no pointer
    /// row is written. The played-state sync reuses it instead of reading the hierarchy again.
    pub(crate) container_scope: Option<PlaybackContainerScope>,
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

/// Row of every [`StoredWebPlaybackSession`] projection.
///
/// `is_admin` stays `i64` because both backends store the flag as `BIGINT` 0/1 and the conversion
/// to `bool` happens in [`stored_web_playback_session`].
#[derive(Debug, sqlx::FromRow)]
struct StoredWebPlaybackSessionRow {
    id: String,
    user_id: String,
    item_id: String,
    media_source_id: Option<String>,
    play_session_id: String,
    tier: i64,
    plan: String,
    state: String,
    temp_dir: Option<String>,
    is_admin: i64,
    expires_at: i64,
    last_heartbeat_at: i64,
    last_sequence: i64,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    video_bitrate: Option<i64>,
    audio_bitrate: Option<i64>,
    transcoding_container: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn stored_web_playback_session(
    row: sqlx::any::AnyRow,
) -> Result<StoredWebPlaybackSession, sqlx::Error> {
    let row = StoredWebPlaybackSessionRow::from_row(&row)?;
    Ok(StoredWebPlaybackSession {
        id: row.id,
        user_id: row.user_id,
        item_id: row.item_id,
        media_source_id: row.media_source_id,
        play_session_id: row.play_session_id,
        tier: row.tier,
        plan: row.plan,
        state: row.state,
        temp_dir: row.temp_dir,
        is_admin: row.is_admin != 0,
        expires_at: row.expires_at,
        last_heartbeat_at: row.last_heartbeat_at,
        last_sequence: row.last_sequence,
        video_codec: row.video_codec,
        audio_codec: row.audio_codec,
        video_bitrate: row.video_bitrate,
        audio_bitrate: row.audio_bitrate,
        transcoding_container: row.transcoding_container,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
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

/// Duration candidates for one playback callback, read without the item detail projection.
///
/// [`Database::find_catalog_rows`] joins sources, streams, image tags and catalog details only to
/// read a runtime, and every playback callback needs nothing but these four values. Each field is
/// the raw column value, so the caller keeps the established precedence and the "zero means
/// unknown" filtering.
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::FromRow)]
pub(crate) struct StoredPlaybackDurations {
    pub(crate) runtime_ticks: Option<i64>,
    pub(crate) requested_source_ticks: Option<i64>,
    pub(crate) default_source_ticks: Option<i64>,
    pub(crate) first_source_ticks: Option<i64>,
}

/// Row of every [`StoredPlaybackSession`] projection.
///
/// `is_paused` stays `i64` because both backends store the flag as `BIGINT` 0/1 and the conversion
/// to `bool` happens in [`stored_playback_session`].
#[derive(Debug, sqlx::FromRow)]
struct StoredPlaybackSessionRow {
    id: String,
    user_id: String,
    item_id: String,
    media_source_id: Option<String>,
    play_session_id: String,
    device_id: String,
    client: Option<String>,
    device_name: Option<String>,
    client_version: Option<String>,
    device_type: Option<String>,
    remote_ip: Option<String>,
    state: String,
    position_ticks: i64,
    duration_ticks: Option<i64>,
    is_paused: i64,
    started_at: i64,
    last_event_at: i64,
}

fn stored_playback_session(row: sqlx::any::AnyRow) -> Result<StoredPlaybackSession, sqlx::Error> {
    let row = StoredPlaybackSessionRow::from_row(&row)?;
    Ok(StoredPlaybackSession {
        id: row.id,
        user_id: row.user_id,
        item_id: row.item_id,
        media_source_id: row.media_source_id,
        play_session_id: row.play_session_id,
        device_id: row.device_id,
        client: row.client,
        device_name: row.device_name,
        client_version: row.client_version,
        device_type: row.device_type,
        remote_ip: row.remote_ip,
        state: row.state,
        position_ticks: row.position_ticks,
        duration_ticks: row.duration_ticks,
        is_paused: row.is_paused != 0,
        started_at: row.started_at,
        last_event_at: row.last_event_at,
    })
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

/// Row of every [`StoredItemImage`] projection; `root_path` comes from the joined library root.
#[derive(Debug, sqlx::FromRow)]
struct StoredItemImageRow {
    id: String,
    item_id: String,
    image_type: String,
    image_index: i64,
    local_path: String,
    file_size: Option<i64>,
    content_tag: Option<String>,
    source: String,
    root_path: Option<String>,
}

fn stored_item_image(row: sqlx::any::AnyRow) -> Result<StoredItemImage, sqlx::Error> {
    let row = StoredItemImageRow::from_row(&row)?;
    Ok(StoredItemImage {
        id: row.id,
        item_id: row.item_id,
        image_type: row.image_type,
        image_index: row.image_index,
        local_path: row.local_path,
        file_size: row.file_size,
        content_tag: row.content_tag,
        source: row.source,
        root_path: row.root_path,
    })
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

/// Row of every [`StoredDanmakuMatchJob`] projection.
///
/// `overwrite` and `cancel_requested` stay `i64` because both backends store the flags as
/// `BIGINT` 0/1 and the conversion to `bool` happens in [`stored_danmaku_match_job`].
#[derive(Debug, sqlx::FromRow)]
struct StoredDanmakuMatchJobRow {
    id: String,
    library_id: String,
    status: String,
    overwrite: i64,
    concurrency: i64,
    total_count: i64,
    processed_count: i64,
    success_count: i64,
    skipped_count: i64,
    failed_count: i64,
    cancel_requested: i64,
    error: Option<String>,
    created_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
}

fn stored_danmaku_match_job(row: sqlx::any::AnyRow) -> Result<StoredDanmakuMatchJob, sqlx::Error> {
    let row = StoredDanmakuMatchJobRow::from_row(&row)?;
    Ok(StoredDanmakuMatchJob {
        id: row.id,
        library_id: row.library_id,
        status: row.status,
        overwrite: row.overwrite != 0,
        concurrency: row.concurrency,
        total_count: row.total_count,
        processed_count: row.processed_count,
        success_count: row.success_count,
        skipped_count: row.skipped_count,
        failed_count: row.failed_count,
        cancel_requested: row.cancel_requested != 0,
        error: row.error,
        created_at: row.created_at,
        started_at: row.started_at,
        finished_at: row.finished_at,
    })
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

/// Row of every [`StoredLibraryCoverJob`] projection.
///
/// `is_manual` stays `i64` because both backends store the flag as `BIGINT` 0/1 and the conversion
/// to `bool` happens in [`stored_library_cover_job`].
#[derive(Debug, sqlx::FromRow)]
struct StoredLibraryCoverJobRow {
    id: String,
    library_id: String,
    is_manual: i64,
    status: String,
    processed_count: i64,
    total_count: i64,
    error: Option<String>,
    created_at: i64,
    updated_at: i64,
    started_at: Option<i64>,
    finished_at: Option<i64>,
}

fn stored_library_cover_job(row: sqlx::any::AnyRow) -> Result<StoredLibraryCoverJob, sqlx::Error> {
    let row = StoredLibraryCoverJobRow::from_row(&row)?;
    Ok(StoredLibraryCoverJob {
        id: row.id,
        library_id: row.library_id,
        is_manual: row.is_manual != 0,
        status: row.status,
        processed_count: row.processed_count,
        total_count: row.total_count,
        error: row.error,
        created_at: row.created_at,
        updated_at: row.updated_at,
        started_at: row.started_at,
        finished_at: row.finished_at,
    })
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
///
/// Statements without a `?` byte never touch the memo: they are not rewritten, keep their static
/// pointer and cost a single linear scan for `?` with no lock and no allocation.
fn adapt_static_sql_for_backend_with_cache(
    cache: &AdaptedSqlCache,
    sql: &'static str,
) -> sqlx::SqlStr {
    if !sql.as_bytes().contains(&b'?') {
        return sqlx::SqlStr::from_static(sql);
    }
    if sql.len() > cache.max_source_bytes {
        return adapt_postgres_sql_uncached(sql);
    }

    let key = StaticSqlKey::of(sql);
    if let Some(rewritten) = cache.lookup(key) {
        return sqlx::SqlSafeStr::into_sql_str(sqlx::AssertSqlSafe(rewritten));
    }

    // The fast path above guarantees a `?` byte, so the rewrite always produces text.
    let Some(rewritten) = rewrite_postgres_placeholders(sql) else {
        return sqlx::SqlStr::from_static(sql);
    };
    let rewritten: Arc<str> = Arc::from(rewritten.as_str());
    cache.store(key, Arc::clone(&rewritten));
    sqlx::SqlSafeStr::into_sql_str(sqlx::AssertSqlSafe(rewritten))
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

/// Number of independently locked memo shards; the count is rounded up to a power of two.
const ADAPTED_SQL_CACHE_SHARDS: usize = 64;
/// Upper bound on memoized statements. The production tree passes ~1,124 distinct static
/// statements to `Database::query*` (counted in docs/PERFORMANCE.md), so this is ~7x headroom: the
/// statement set is fixed at build time, the memo fills once and then only serves reads.
const MAX_ADAPTED_SQL_CACHE_ENTRIES: usize = 8_192;
/// Upper bound on the rewritten text the memo keeps alive. Those same statements rewrite to well
/// under 1 MiB, so this is safety headroom rather than a working limit.
const MAX_ADAPTED_SQL_CACHE_TEXT_BYTES: usize = 8 * 1024 * 1024;
/// Source statements longer than this are rewritten on every call instead of being memoized. The
/// limit is compared against the source length, before any rewrite work happens.
const MAX_ADAPTED_SQL_CACHE_SOURCE_BYTES: usize = 64 * 1024;
/// Odd multiplicative constant used to spread statement addresses over the shards (the 64-bit
/// golden-ratio word, truncated on 32-bit targets).
const ADAPTED_SQL_CACHE_SHARD_MULTIPLIER: usize = 0x9E37_79B9_7F4A_7C15_u64 as usize;

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

/// One shard of the memo: rewritten statement text keyed by statement identity.
#[derive(Default)]
struct AdaptedSqlShard {
    entries: HashMap<StaticSqlKey, Arc<str>>,
    text_bytes: usize,
}

/// Append-only memo of PostgreSQL placeholder rewrites for `&'static str` statements.
///
/// There is no eviction policy, and none is needed: the set of static statements is fixed when the
/// binary is built, so the memo fills once and afterwards only serves reads. Capacity is enforced
/// with per-shard limits that sum to the global limits; a statement that does not fit keeps taking
/// the uncached path instead of costing an eviction scan on the miss path.
///
/// A lock is held for one hash-map operation on one shard: no await, no caller code and never two
/// shards at once, so the memo cannot deadlock. A poisoned shard degrades to a miss (read) or
/// skips the insert (write), because rewriting is always correct.
struct AdaptedSqlCache {
    shards: Box<[std::sync::RwLock<AdaptedSqlShard>]>,
    shard_mask: usize,
    max_entries_per_shard: usize,
    max_text_bytes_per_shard: usize,
    max_source_bytes: usize,
}

static ADAPTED_SQL_CACHE: std::sync::LazyLock<AdaptedSqlCache> =
    std::sync::LazyLock::new(AdaptedSqlCache::default);

impl Default for AdaptedSqlCache {
    fn default() -> Self {
        Self::with_limits(
            MAX_ADAPTED_SQL_CACHE_ENTRIES,
            MAX_ADAPTED_SQL_CACHE_TEXT_BYTES,
            MAX_ADAPTED_SQL_CACHE_SOURCE_BYTES,
        )
    }
}

impl AdaptedSqlCache {
    /// Cache whose global limits are spread evenly over [`ADAPTED_SQL_CACHE_SHARDS`] shards.
    fn with_limits(entries: usize, text_bytes: usize, source_bytes: usize) -> Self {
        Self::with_shard_limits(
            ADAPTED_SQL_CACHE_SHARDS,
            entries.div_ceil(ADAPTED_SQL_CACHE_SHARDS),
            text_bytes.div_ceil(ADAPTED_SQL_CACHE_SHARDS),
            source_bytes,
        )
    }

    /// Cache with explicit per-shard limits; tests use small values to reach them quickly.
    fn with_shard_limits(
        shards: usize,
        entries_per_shard: usize,
        text_bytes_per_shard: usize,
        source_bytes: usize,
    ) -> Self {
        // Rounding up keeps the shard index a mask instead of a division, and cannot go out of
        // bounds whatever the caller passes.
        let shards = shards.max(1).next_power_of_two();
        Self {
            shards: (0..shards)
                .map(|_| std::sync::RwLock::new(AdaptedSqlShard::default()))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            shard_mask: shards - 1,
            max_entries_per_shard: entries_per_shard,
            max_text_bytes_per_shard: text_bytes_per_shard,
            max_source_bytes: source_bytes,
        }
    }

    /// Statements live in read-only data, so their addresses are stable and distinct.
    ///
    /// The address is multiplied and folded before masking: real statement addresses are often a
    /// regular stride apart (consecutive string literals, or the heap blocks of a benchmark's leaked
    /// statements), and masking the raw address would pile those onto a handful of shards until
    /// their entry limits stopped the memo from caching them at all.
    fn shard_index(&self, key: StaticSqlKey) -> usize {
        let mixed = key.address.wrapping_mul(ADAPTED_SQL_CACHE_SHARD_MULTIPLIER);
        (mixed ^ (mixed >> 29)) & self.shard_mask
    }

    /// Returns the memoized rewrite of `key`, taking only a read lock on its shard.
    fn lookup(&self, key: StaticSqlKey) -> Option<Arc<str>> {
        let guard = self.shards[self.shard_index(key)].read().ok()?;
        guard.entries.get(&key).map(Arc::clone)
    }

    /// Memoizes one rewrite, or leaves it out when this shard is already full.
    fn store(&self, key: StaticSqlKey, rewritten: Arc<str>) {
        let Ok(mut guard) = self.shards[self.shard_index(key)].write() else {
            return;
        };
        if guard.entries.contains_key(&key) {
            return;
        }
        if guard.entries.len() >= self.max_entries_per_shard
            || guard.text_bytes.saturating_add(rewritten.len()) > self.max_text_bytes_per_shard
        {
            return;
        }
        guard.text_bytes = guard.text_bytes.saturating_add(rewritten.len());
        guard.entries.insert(key, rewritten);
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| {
                shard
                    .read()
                    .map(|shard| shard.entries.len())
                    .unwrap_or_default()
            })
            .sum()
    }

    #[cfg(test)]
    fn cached_text_bytes(&self) -> usize {
        self.shards
            .iter()
            .map(|shard| {
                shard
                    .read()
                    .map(|shard| shard.text_bytes)
                    .unwrap_or_default()
            })
            .sum()
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

/// Drives the `stored_*` decoders through the query paths that share them.
///
/// Each decoder has one named row struct, so a column rename or type change breaks a test
/// here instead of panicking inside a request.
#[cfg(test)]
mod stored_row_decoding_tests {
    use super::*;
    use crate::storage::test_database;

    #[tokio::test]
    async fn stored_user_row_decodes_the_user_projection() -> Result<(), Box<dyn std::error::Error>>
    {
        let (_temp_dir, database) = test_database().await?;
        database
            .insert_user("user-alice", "alice", "Alice", "hash", false, true)
            .await?;

        let user = database
            .find_user_by_id("user-alice")
            .await?
            .expect("user should exist");
        assert_eq!(user.id, "user-alice");
        assert_eq!(user.username_normalized, "alice");
        assert_eq!(user.display_name, "Alice");
        assert_eq!(user.password_hash, "hash");
        assert!(user.has_password);
        assert!(!user.is_admin);
        assert!(!user.is_disabled);
        assert_eq!(user.email, None);
        assert_eq!(user.last_login_at, None);
        Ok(())
    }

    #[tokio::test]
    async fn stored_row_reports_a_missing_column_instead_of_panicking()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let row = sqlx::query("SELECT 1 AS other")
            .fetch_one(database.pool())
            .await?;
        let error = StoredUserRow::from_row(&row).expect_err("missing columns must not panic");
        assert!(
            matches!(error, sqlx::Error::ColumnNotFound(_)),
            "unexpected decode error: {error}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn stored_admin_user_list_item_row_decodes_the_admin_projection()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        database
            .insert_user("user-admin", "admin", "Admin", "hash", true, true)
            .await?;
        database
            .insert_user("user-viewer", "viewer", "Viewer", "hash", false, true)
            .await?;

        let (users, total) = database
            .query_admin_users(
                AdminUserStatusFilter::Active,
                None,
                AdminUserSortField::Created,
                false,
                50,
                0,
            )
            .await?;
        assert_eq!(total, 2);
        assert_eq!(users.len(), 2);
        let admin = users
            .iter()
            .find(|user| user.id == "user-admin")
            .expect("admin should be listed");
        assert_eq!(admin.display_name, "Admin");
        assert!(admin.is_admin);
        assert!(admin.has_password);
        assert_eq!(admin.email, None);
        assert_eq!(admin.last_activity_at, None);
        assert_eq!(admin.current_playback_count, 0);
        let viewer = users
            .iter()
            .find(|user| user.id == "user-viewer")
            .expect("viewer should be listed");
        assert!(!viewer.is_admin);
        assert_eq!(viewer.last_login_at, None);
        Ok(())
    }

    #[tokio::test]
    async fn stored_scheduled_task_rows_decode_configs_and_plans()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        sqlx::query(
            "INSERT INTO scheduled_task_plans (
                 id, task_type, plan_name, task_name, task_description,
                 source_type, cron_or_interval, scope_type, is_default
             ) VALUES (
                 'plan-a', 'INCREMENTAL_SCAN', 'Plan A', 'Scan', 'Incremental scan',
                 'SYSTEM', '0 3 * * *', 'GLOBAL', 1
             )",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO scheduled_task_configs (
                 owner_type, owner_id, task_type, plan_id, cron_or_interval, is_enabled
             ) VALUES ('GLOBAL', 'global', 'INCREMENTAL_SCAN', 'plan-a', '0 3 * * *', 1)",
        )
        .execute(database.pool())
        .await?;

        let config = database
            .find_scheduled_task_config("GLOBAL", "global", "INCREMENTAL_SCAN")
            .await?
            .expect("config should exist");
        assert_eq!(config.owner_type, "GLOBAL");
        assert_eq!(config.owner_id, "global");
        assert_eq!(config.plan_id.as_deref(), Some("plan-a"));
        assert_eq!(config.task_name, "");
        assert_eq!(config.source_type, "SYSTEM");
        assert!(config.is_enabled);
        assert_eq!(config.library_name, None);

        let plan = database
            .find_scheduled_task_plan("plan-a")
            .await?
            .expect("plan should exist");
        assert_eq!(plan.task_type, "INCREMENTAL_SCAN");
        assert_eq!(plan.plan_name, "Plan A");
        assert_eq!(plan.cron_or_interval.as_deref(), Some("0 3 * * *"));
        assert!(plan.is_enabled);
        assert!(plan.is_default);
        assert!(plan.libraries.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn stored_notification_rows_decode_destinations_and_deliveries()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        database
            .notifications()
            .create_notification_destination(NewNotificationDestination {
                id: "destination-a",
                name: "Webhook",
                url: "https://example.test/hook",
                enabled: true,
                allow_private_network: false,
                event_types_json: "[\"ITEM_ADDED\"]",
                payload_format: "LUX",
                provider_plugin_id: "builtin.webhook",
                provider_config_json: "{}",
            })
            .await?;

        let destination = database
            .notifications()
            .find_notification_destination("destination-a")
            .await?
            .expect("destination should exist");
        assert_eq!(destination.name, "Webhook");
        assert_eq!(destination.url, "https://example.test/hook");
        assert!(destination.enabled);
        assert!(!destination.allow_private_network);
        assert_eq!(destination.event_types_json, "[\"ITEM_ADDED\"]");
        assert_eq!(destination.payload_format, "LUX");

        sqlx::query(
            "INSERT INTO notification_events (
                 id, event_type, schema_version, occurred_at, dedupe_key, payload_json
             ) VALUES ('event-a', 'ITEM_ADDED', 1, 1800000000, 'dedupe-a', '{}')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO notification_deliveries (
                 id, event_id, destination_id, status, attempt_count, next_attempt_at
             ) VALUES ('delivery-a', 'event-a', 'destination-a', 'PENDING', 0, unixepoch())",
        )
        .execute(database.pool())
        .await?;

        let deliveries = database
            .notifications()
            .list_ready_notification_deliveries(10)
            .await?;
        assert_eq!(deliveries.len(), 1);
        let delivery = &deliveries[0];
        assert_eq!(delivery.id, "delivery-a");
        assert_eq!(delivery.event_id, "event-a");
        assert_eq!(delivery.destination_id, "destination-a");
        assert_eq!(delivery.status, "PENDING");
        assert_eq!(delivery.attempt_count, 0);
        assert_eq!(delivery.last_http_status, None);
        assert_eq!(delivery.event_type, "ITEM_ADDED");
        assert_eq!(delivery.payload_json, "{}");
        assert_eq!(delivery.destination_name, "Webhook");
        assert_eq!(delivery.destination_url, "https://example.test/hook");
        assert!(!delivery.allow_private_network);
        assert_eq!(delivery.provider_plugin_id, "builtin.webhook");
        Ok(())
    }

    /// Covers the STRM probe job decoder, which has no other library test path.
    #[tokio::test]
    async fn stored_strm_probe_job_row_decodes_the_job_projection()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('strm-lib', 'STRM', 'MOVIE')")
            .execute(database.pool())
            .await?;
        database
            .create_strm_probe_job(NewStrmProbeJob {
                id: "probe-job",
                operation_id: "probe-operation",
                library_id: "strm-lib",
                concurrency: 2,
                include_ready: true,
                write_sidecars: false,
                media_info_enabled: true,
                thumbnail_enabled: false,
                thumbnail_position_percent: 25,
                target_scan_job_id: None,
                target_source_id: None,
                total_count: 3,
            })
            .await?;

        let job = database
            .find_strm_probe_job("probe-job")
            .await?
            .expect("probe job should exist");
        assert_eq!(job.operation_id, "probe-operation");
        assert_eq!(job.library_id, "strm-lib");
        assert_eq!(job.status, "PENDING");
        assert_eq!(job.concurrency, 2);
        assert!(job.include_ready);
        assert!(!job.write_sidecars);
        assert!(job.media_info_enabled);
        assert!(!job.thumbnail_enabled);
        assert_eq!(job.thumbnail_position_percent, 25);
        assert_eq!(job.target_scan_job_id, None);
        assert_eq!(job.target_source_id, None);
        assert_eq!(job.cursor, None);
        assert_eq!(job.processed_count, 0);
        assert_eq!(job.total_count, 3);
        assert!(!job.cancel_requested);
        assert_eq!(job.error, None);
        assert_eq!(job.started_at, None);
        assert_eq!(job.finished_at, None);
        Ok(())
    }

    #[tokio::test]
    async fn stored_library_root_and_filesystem_entry_rows_decode_existing_paths()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('root-lib', 'Roots', 'MOVIE')")
            .execute(database.pool())
            .await?;
        database
            .insert_library_root(NewLibraryRoot {
                id: "root-a",
                library_id: "root-lib",
                canonical_path: "/media/root-a",
                display_path: "Root A",
                is_available: true,
                is_writable: false,
            })
            .await?;

        let root = database
            .find_library_root("root-a")
            .await?
            .expect("library root should exist");
        assert_eq!(root.library_id, "root-lib");
        assert_eq!(root.canonical_path, "/media/root-a");
        assert_eq!(root.display_path, "Root A");
        assert!(root.is_available);
        assert!(!root.is_writable);
        assert_eq!(root.unavailable_since, None);
        assert_eq!(root.scan_cursor, None);

        database
            .insert_filesystem_entry(NewFilesystemEntry {
                id: "entry-a",
                library_root_id: "root-a",
                relative_path: "movies/a.mkv",
                entry_kind: "FILE",
                size: 1024,
                modified_at: 1_800_000_000,
                device: Some(7),
                inode: Some(11),
                fingerprint: b"fingerprint-a",
                last_seen_generation: "generation-a",
            })
            .await?;
        let entry = database
            .find_filesystem_entry("root-a", "movies/a.mkv")
            .await?
            .expect("filesystem entry should exist");
        assert_eq!(entry.entry_kind, "FILE");
        assert_eq!(entry.size, 1024);
        assert_eq!(entry.modified_at, 1_800_000_000);
        assert_eq!(entry.device, Some(7));
        assert_eq!(entry.inode, Some(11));
        assert_eq!(entry.fingerprint.as_deref(), Some(&b"fingerprint-a"[..]));
        assert_eq!(entry.last_seen_generation, "generation-a");
        assert!(!entry.is_missing);
        assert_eq!(entry.item_id, None);
        assert_eq!(entry.parent_identity_key, None);
        assert_eq!(entry.item_type, None);
        assert_eq!(entry.item_identity_key, None);
        assert_eq!(entry.series_provider_ids_json, None);
        Ok(())
    }

    #[tokio::test]
    async fn stored_person_index_rebuild_job_row_decodes_the_queue_projection()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        sqlx::query(
            "INSERT INTO libraries (id, name, kind, is_enabled)
             VALUES ('rebuild-lib', 'Rebuild', 'MOVIE', 1)",
        )
        .execute(database.pool())
        .await?;

        let jobs = database.sync_person_index_rebuild_jobs(1).await?;
        let queued = jobs
            .iter()
            .find(|job| job.library_id == "rebuild-lib")
            .expect("rebuild job should be queued");
        assert_eq!(queued.status, "QUEUED");
        assert_eq!(queued.cursor_id, None);
        assert_eq!(queued.processed_count, 0);
        assert_eq!(queued.total_count, 0);
        assert!(!queued.cancel_requested);

        let stored = database
            .get_person_index_rebuild_job("rebuild-lib")
            .await?
            .expect("rebuild job should be readable");
        assert_eq!(stored.library_id, "rebuild-lib");
        assert_eq!(stored.status, "QUEUED");
        assert!(!stored.cancel_requested);
        Ok(())
    }

    /// A projection that drifts from its row struct must surface as a storage error.
    ///
    /// The shared decoders used to call `row.get`, which panicked on a missing column; every
    /// `*Row` struct reports `ColumnNotFound` instead.
    #[tokio::test]
    async fn stored_row_structs_report_missing_columns_instead_of_panicking()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let row = sqlx::query("SELECT 1 AS other")
            .fetch_one(database.pool())
            .await?;

        let errors = [
            StoredScanJobRow::from_row(&row).expect_err("scan job columns must not decode"),
            StoredScanLocalMetadataBatchRow::from_row(&row)
                .expect_err("local metadata batch columns must not decode"),
            StoredScanJobPathRow::from_row(&row)
                .expect_err("scan job path columns must not decode"),
            StoredReconciliationScanEntryRow::from_row(&row)
                .expect_err("reconciliation entry columns must not decode"),
            StoredStrmProbeJobRow::from_row(&row)
                .expect_err("STRM probe job columns must not decode"),
            StoredChapterDetectionJobRow::from_row(&row)
                .expect_err("chapter detection job columns must not decode"),
            StoredLibraryRootRow::from_row(&row).expect_err("library root columns must not decode"),
            StoredFilesystemEntryRow::from_row(&row)
                .expect_err("filesystem entry columns must not decode"),
            StoredCanonicalPersonRow::from_row(&row)
                .expect_err("canonical person columns must not decode"),
            StoredPersonMatchCandidateRow::from_row(&row)
                .expect_err("person match candidate columns must not decode"),
            StoredPersonCreditRow::from_row(&row)
                .expect_err("person credit columns must not decode"),
            StoredPersonIndexRebuildJobRow::from_row(&row)
                .expect_err("person index rebuild job columns must not decode"),
            StoredMetadataReidentifyJobRow::from_row(&row)
                .expect_err("metadata reidentify job columns must not decode"),
            StoredMetadataReidentifyItemRow::from_row(&row)
                .expect_err("metadata reidentify item columns must not decode"),
            StoredMetadataCandidateRow::from_row(&row)
                .expect_err("metadata candidate columns must not decode"),
            StoredItemMetadataCompletenessRow::from_row(&row)
                .expect_err("item metadata completeness columns must not decode"),
            StoredMediaItemRow::from_row(&row).expect_err("media item columns must not decode"),
            StoredWebPlaybackSessionRow::from_row(&row)
                .expect_err("web playback session columns must not decode"),
            StoredPlaybackSessionRow::from_row(&row)
                .expect_err("playback session columns must not decode"),
            StoredItemImageRow::from_row(&row).expect_err("item image columns must not decode"),
            StoredDanmakuMatchJobRow::from_row(&row)
                .expect_err("danmaku match job columns must not decode"),
            StoredLibraryCoverJobRow::from_row(&row)
                .expect_err("library cover job columns must not decode"),
            RecommendationRatingMedianRow::from_row(&row)
                .expect_err("rating median columns must not decode"),
            SchemaVersionRow::from_row(&row).expect_err("schema version columns must not decode"),
        ];
        for error in errors {
            assert!(
                matches!(error, sqlx::Error::ColumnNotFound(_)),
                "unexpected decode error: {error}"
            );
        }
        Ok(())
    }

    /// The rating-median cache read is covered by
    /// `recommendation_rating_median_survives_restart_until_thirty_day_ttl`; the schema version
    /// projection has no other library test path.
    #[tokio::test]
    async fn schema_version_row_decodes_the_migration_projection()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        assert!(database.schema_version().await? > 0);
        Ok(())
    }

    #[tokio::test]
    async fn stored_metadata_reidentify_rows_decode_jobs_and_items()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('reid-lib', 'Reid', 'MOVIE')")
            .execute(database.pool())
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (
                 'reid-item', 'reid-lib', 'MOVIE', 'Reid Movie', 'reid movie', 'LOCAL_CONFIRMED'
             )",
        )
        .execute(database.pool())
        .await?;
        database
            .create_metadata_reidentify_job("reid-job", &["reid-item".to_owned()], "REIDENTIFY")
            .await?;

        let job = database
            .find_metadata_reidentify_job("reid-job")
            .await?
            .expect("reidentify job should exist");
        assert_eq!(job.status, "QUEUED");
        assert_eq!(job.mode, "REIDENTIFY");
        assert_eq!(job.job_scope, "ITEMS");
        assert_eq!(job.library_id.as_deref(), Some("reid-lib"));
        assert_eq!(job.total_count, 1);
        assert_eq!(job.processed_count, 0);
        assert_eq!(job.pending_count, 0);
        assert!(!job.cancel_requested);
        assert_eq!(job.error, None);
        assert_eq!(job.started_at, None);
        assert_eq!(job.finished_at, None);

        let items = database
            .list_metadata_reidentify_items("reid-job", 0, 10)
            .await?;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].job_id, "reid-job");
        assert_eq!(items[0].item_id, "reid-item");
        assert_eq!(items[0].status, "PENDING");
        assert_eq!(items[0].candidate_count, 0);
        assert_eq!(items[0].error, None);
        Ok(())
    }

    #[tokio::test]
    async fn stored_metadata_candidate_and_completeness_rows_decode_the_projections()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('meta-lib', 'Meta', 'MOVIE')")
            .execute(database.pool())
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (
                 'candidate-item', 'meta-lib', 'MOVIE', 'Candidate Movie',
                 'candidate movie', 'LOCAL_CONFIRMED'
             )",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO metadata_candidates (
                 id, item_id, provider, provider_id, candidate_json, score, status,
                 expires_at
             ) VALUES (
                 'candidate-a', 'candidate-item', 'tmdb', '42', '{\"title\":\"Candidate\"}',
                 87.5, 'PENDING', 1900000000
             )",
        )
        .execute(database.pool())
        .await?;

        let candidate = database
            .find_metadata_candidate("candidate-item", "candidate-a")
            .await?
            .expect("candidate should exist");
        assert_eq!(candidate.id, "candidate-a");
        assert_eq!(candidate.item_id, "candidate-item");
        assert_eq!(candidate.provider, "tmdb");
        assert_eq!(candidate.provider_id, "42");
        assert_eq!(candidate.score, 87.5);
        assert_eq!(candidate.status, "PENDING");
        assert_eq!(candidate.expires_at, Some(1_900_000_000));
        assert_eq!(candidate.item_title, "Candidate Movie");

        sqlx::query(
            "INSERT INTO item_metadata_completeness (
                 item_id, capability, local_state, is_missing, input_fingerprint,
                 checked_at, retry_after, error
             ) VALUES (
                 'candidate-item', 'metadata', 'READY', 1, X'63616665', 1700000000, NULL, NULL
             )",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO item_metadata_completeness (
                 item_id, capability, local_state, is_missing, input_fingerprint,
                 checked_at, retry_after, error
             ) VALUES (
                 'candidate-item', 'images', 'PENDING', NULL, NULL, NULL, 1700000123, 'queued'
             )",
        )
        .execute(database.pool())
        .await?;

        let missing = database
            .find_item_metadata_completeness("candidate-item", "metadata")
            .await?
            .expect("metadata completeness should exist");
        assert_eq!(missing.local_state, "READY");
        assert_eq!(missing.is_missing, Some(true));
        assert_eq!(missing.input_fingerprint.as_deref(), Some(&b"cafe"[..]));
        assert_eq!(missing.checked_at, Some(1_700_000_000));
        assert_eq!(missing.retry_after, None);
        assert_eq!(missing.error, None);

        let pending = database
            .find_item_metadata_completeness("candidate-item", "images")
            .await?
            .expect("image completeness should exist");
        assert_eq!(pending.local_state, "PENDING");
        assert_eq!(pending.is_missing, None);
        assert_eq!(pending.checked_at, None);
        assert_eq!(pending.retry_after, Some(1_700_000_123));
        assert_eq!(pending.error.as_deref(), Some("queued"));
        Ok(())
    }

    #[tokio::test]
    async fn stored_media_item_and_item_image_rows_decode_the_projections()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        sqlx::query(
            "INSERT INTO libraries (id, name, kind) VALUES ('image-lib', 'Image', 'MOVIE')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('image-root', 'image-lib', '/media/images', 'Images', 1, 1)",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status, identity_key
             ) VALUES (
                 'image-item', 'image-lib', 'MOVIE', 'Image Movie', 'image movie',
                 'LOCAL_CONFIRMED', 'identity-image'
             )",
        )
        .execute(database.pool())
        .await?;

        let media_item = database
            .find_media_item_by_identity("identity-image")
            .await?
            .expect("media item should exist");
        assert_eq!(media_item.id, "image-item");

        sqlx::query(
            "INSERT INTO item_images (
                 id, item_id, image_type, image_index, local_path, file_size, content_tag, source
             ) VALUES (
                 'image-a', 'image-item', 'POSTER', 1, 'posters/image-a.jpg', 2048, 'tag-a', 'LOCAL'
             )",
        )
        .execute(database.pool())
        .await?;

        let image = database
            .find_item_image("image-item", "image-a")
            .await?
            .expect("item image should exist");
        assert_eq!(image.id, "image-a");
        assert_eq!(image.item_id, "image-item");
        assert_eq!(image.image_type, "POSTER");
        assert_eq!(image.image_index, 1);
        assert_eq!(image.local_path, "posters/image-a.jpg");
        assert_eq!(image.file_size, Some(2048));
        assert_eq!(image.content_tag.as_deref(), Some("tag-a"));
        assert_eq!(image.source, "LOCAL");
        assert_eq!(image.root_path.as_deref(), Some("/media/images"));
        Ok(())
    }

    #[tokio::test]
    async fn stored_danmaku_match_and_library_cover_job_rows_decode_the_projections()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('job-lib', 'Jobs', 'MOVIE')")
            .execute(database.pool())
            .await?;
        database
            .create_danmaku_match_job(NewDanmakuMatchJob {
                id: "danmaku-job",
                library_id: "job-lib",
                overwrite: true,
                concurrency: 3,
            })
            .await?;

        let danmaku = database
            .find_danmaku_match_job("danmaku-job")
            .await?
            .expect("danmaku job should exist");
        assert_eq!(danmaku.id, "danmaku-job");
        assert_eq!(danmaku.library_id, "job-lib");
        assert_eq!(danmaku.status, "PENDING");
        assert!(danmaku.overwrite);
        assert_eq!(danmaku.concurrency, 3);
        assert_eq!(danmaku.total_count, 0);
        assert_eq!(danmaku.processed_count, 0);
        assert_eq!(danmaku.success_count, 0);
        assert_eq!(danmaku.skipped_count, 0);
        assert_eq!(danmaku.failed_count, 0);
        assert!(!danmaku.cancel_requested);
        assert_eq!(danmaku.error, None);
        assert_eq!(danmaku.started_at, None);
        assert_eq!(danmaku.finished_at, None);

        assert!(
            database
                .create_library_cover_job("cover-job", "job-lib", true)
                .await?
        );
        let cover = database
            .find_library_cover_job("cover-job")
            .await?
            .expect("library cover job should exist");
        assert_eq!(cover.id, "cover-job");
        assert_eq!(cover.library_id, "job-lib");
        assert!(cover.is_manual);
        assert_eq!(cover.status, "PENDING");
        assert_eq!(cover.processed_count, 0);
        assert_eq!(cover.total_count, 1);
        assert_eq!(cover.error, None);
        assert_eq!(cover.started_at, None);
        assert_eq!(cover.finished_at, None);
        Ok(())
    }

    #[tokio::test]
    async fn stored_playback_and_web_playback_session_rows_decode_the_projections()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        database
            .insert_user("session-user", "session", "Session", "hash", false, true)
            .await?;
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('session-lib', 'S', 'MOVIE')")
            .execute(database.pool())
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (
                 'session-item', 'session-lib', 'MOVIE', 'Session Movie',
                 'session movie', 'LOCAL_CONFIRMED'
             )",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO playback_sessions (
                 id, user_id, item_id, play_session_id, device_id, client, device_name,
                 client_version, device_type, remote_ip, state, position_ticks,
                 duration_ticks, is_paused, started_at, last_event_at
             ) VALUES (
                 'ps-a', 'session-user', 'session-item', 'play-a', 'device-a', 'Lux',
                 'Living Room', '1.2.3', 'TV', '10.0.0.7', 'PAUSED', 120, 240, 1,
                 1800000000, 1800000060
             )",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO web_playback_sessions (
                 id, user_id, item_id, media_source_id, play_session_id, tier, plan, state,
                 temp_dir, is_admin, expires_at, last_heartbeat_at, last_sequence,
                 video_codec, audio_codec, video_bitrate, audio_bitrate,
                 transcoding_container, created_at, updated_at
             ) VALUES (
                 'wps-a', 'session-user', 'session-item', NULL, 'web-a', 2, 'SERVER_HLS',
                 'ACTIVE', '/tmp/transcode', 1, 1800003600, 1800000060, 7,
                 'h264', 'aac', 4000000, 192000, 'fmp4', 1800000000, 1800000060
             )",
        )
        .execute(database.pool())
        .await?;

        let session = database
            .find_playback_session_by_id("ps-a")
            .await?
            .expect("playback session should exist");
        assert_eq!(session.user_id, "session-user");
        assert_eq!(session.item_id, "session-item");
        assert_eq!(session.media_source_id, None);
        assert_eq!(session.play_session_id, "play-a");
        assert_eq!(session.device_id, "device-a");
        assert_eq!(session.client.as_deref(), Some("Lux"));
        assert_eq!(session.device_name.as_deref(), Some("Living Room"));
        assert_eq!(session.client_version.as_deref(), Some("1.2.3"));
        assert_eq!(session.device_type.as_deref(), Some("TV"));
        assert_eq!(session.remote_ip.as_deref(), Some("10.0.0.7"));
        assert_eq!(session.state, "PAUSED");
        assert_eq!(session.position_ticks, 120);
        assert_eq!(session.duration_ticks, Some(240));
        assert!(session.is_paused);
        assert_eq!(session.started_at, 1_800_000_000);
        assert_eq!(session.last_event_at, 1_800_000_060);

        let web = database
            .find_web_playback_session("wps-a")
            .await?
            .expect("web playback session should exist");
        assert_eq!(web.user_id, "session-user");
        assert_eq!(web.item_id, "session-item");
        assert_eq!(web.play_session_id, "web-a");
        assert_eq!(web.tier, 2);
        assert_eq!(web.plan, "SERVER_HLS");
        assert_eq!(web.state, "ACTIVE");
        assert_eq!(web.temp_dir.as_deref(), Some("/tmp/transcode"));
        assert!(web.is_admin);
        assert_eq!(web.expires_at, 1_800_003_600);
        assert_eq!(web.last_heartbeat_at, 1_800_000_060);
        assert_eq!(web.last_sequence, 7);
        assert_eq!(web.video_codec.as_deref(), Some("h264"));
        assert_eq!(web.audio_codec.as_deref(), Some("aac"));
        assert_eq!(web.video_bitrate, Some(4_000_000));
        assert_eq!(web.audio_bitrate, Some(192_000));
        assert_eq!(web.transcoding_container.as_deref(), Some("fmp4"));
        assert_eq!(web.created_at, 1_800_000_000);
        assert_eq!(web.updated_at, 1_800_000_060);
        Ok(())
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
