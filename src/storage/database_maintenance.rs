use super::*;
use std::time::{Duration, Instant};

/// Maximum number of tables one maintenance round may touch. Keeps the daily
/// pass bounded even on libraries with many large tables.
const MAX_MAINTENANCE_TABLES: usize = 12;
/// Per-table upper bound. PostgreSQL `VACUUM` has no statement timeout of its
/// own, so the round abandons a table that overruns this budget.
const PER_TABLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Default daily period, in seconds.
pub const DEFAULT_MAINTENANCE_INTERVAL_SECONDS: u64 = 86_400;
/// Default delay before the first round, so startup work owns the first minutes.
pub const DEFAULT_MAINTENANCE_INITIAL_DELAY_SECONDS: u64 = 600;

/// Dead tuples must exceed this floor before a table is considered at all.
const DEAD_TUPLE_FLOOR: i64 = 5_000;
/// Default autovacuum trigger is `threshold + scale_factor * live`; only act once
/// dead tuples reach this fraction of that trigger. `0.7` leaves headroom so the
/// pass targets tables that are genuinely stuck rather than merely close.
const TRIGGER_FRACTION_NUMERATOR: i64 = 7;
const TRIGGER_FRACTION_DENOMINATOR: i64 = 10;
const DEFAULT_VACUUM_THRESHOLD: i64 = 50;
const DEFAULT_VACUUM_SCALE_PERCENT: i64 = 20;
const MIN_LIVE_TUPLES_FOR_OVERRIDE: i64 = 10_000;
/// SQLite never reclaims free pages on its own. A full `VACUUM` rewrites the
/// whole database, so only run it when the freelist is both large in absolute
/// terms and a meaningful share of the file.
const SQLITE_VACUUM_MIN_FREELIST_PAGES: i64 = 256;
const SQLITE_VACUUM_MIN_FREELIST_PERCENT: i64 = 5;
/// `PRAGMA auto_vacuum` value for incremental mode. Only databases created by this
/// build reserve it; older databases stay at 0 until an operator runs a full VACUUM.
const SQLITE_AUTO_VACUUM_INCREMENTAL: i64 = 2;
/// Pages truncated per incremental step. Small steps keep the write lock short even
/// when a deletion of a whole library freed hundreds of thousands of pages.
const SQLITE_INCREMENTAL_VACUUM_PAGES_PER_STEP: i64 = 2_048;

const MAINTENANCE_ENV: &str = "LUX_DATABASE_MAINTENANCE";
const MAINTENANCE_INTERVAL_ENV: &str = "LUX_DATABASE_MAINTENANCE_INTERVAL_SECONDS";
const MAINTENANCE_INITIAL_DELAY_ENV: &str = "LUX_DATABASE_MAINTENANCE_INITIAL_DELAY_SECONDS";
/// A full SQLite `VACUUM` rewrites the entire file and holds the write lock for the
/// whole rewrite, so the default budget stays short. Operators who accept the lock
/// for an offline compaction can raise it.
const QUERY_STATISTICS_EXTENSION_ENV: &str = "LUX_POSTGRES_QUERY_STATISTICS_EXTENSION";
const QUERY_STATISTICS_EXTENSION_TIMEOUT: Duration = Duration::from_secs(10);
const SQLITE_VACUUM_TIMEOUT_ENV: &str = "LUX_SQLITE_VACUUM_TIMEOUT_SECONDS";
const DEFAULT_SQLITE_VACUUM_TIMEOUT_SECONDS: u64 = 120;

static MAINTENANCE_RUNNING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// One table selected for maintenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaintenanceCandidate {
    pub table: String,
    pub dead_tuples: i64,
    pub live_tuples: i64,
    /// Whether the row-count source is trustworthy. `pg_stat` reports `0` for
    /// tables that never ran autovacuum or autoanalyze, which can hide hundreds
    /// of thousands of real rows.
    pub statistics_trusted: bool,
}

/// Outcome of one maintenance round.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MaintenanceReport {
    pub vacuumed: Vec<String>,
    pub parameters_applied: Vec<String>,
    pub skipped_untrusted: Vec<String>,
    pub skipped_below_threshold: Vec<String>,
    pub failed: Vec<String>,
}

impl MaintenanceCandidate {
    /// True when live/dead counts came from a source that actually sampled the
    /// table. Empty or barely-used tables must never be judged from the `pg_stat`
    /// placeholder value.
    pub fn can_be_evaluated(&self) -> bool {
        self.statistics_trusted
    }

    /// Whether dead tuples have reached the configured fraction of the default
    /// autovacuum trigger for this table size.
    pub fn exceeds_vacuum_trigger(&self) -> bool {
        if self.dead_tuples < DEAD_TUPLE_FLOOR {
            return false;
        }
        let trigger = DEFAULT_VACUUM_THRESHOLD
            + DEFAULT_VACUUM_SCALE_PERCENT.saturating_mul(self.live_tuples) / 100;
        let scaled =
            trigger.saturating_mul(TRIGGER_FRACTION_NUMERATOR) / TRIGGER_FRACTION_DENOMINATOR;
        self.dead_tuples > scaled
    }

    /// Table-level autovacuum overrides only make sense for tables large enough
    /// that the default 20% threshold can never be reached in practice.
    pub fn deserves_parameter_override(&self) -> bool {
        self.statistics_trusted
            && self.live_tuples >= MIN_LIVE_TUPLES_FOR_OVERRIDE
            && self.exceeds_vacuum_trigger()
    }

    /// SQLite candidate: `dead_tuples` stores freelist pages and
    /// `live_tuples` stores total pages. A handful of freelist pages is not
    /// worth rewriting a multi-gigabyte file.
    pub fn sqlite_freelist_worth_vacuum(&self) -> bool {
        self.statistics_trusted
            && self.dead_tuples >= SQLITE_VACUUM_MIN_FREELIST_PAGES
            && self.live_tuples > 0
            && self.dead_tuples.saturating_mul(100)
                >= self
                    .live_tuples
                    .saturating_mul(SQLITE_VACUUM_MIN_FREELIST_PERCENT)
    }
}

/// Maintenance is on unless explicitly disabled.
/// Resolves the SQLite maintenance budget. Invalid or empty input falls back to the
/// default rather than disabling the bound.
pub(crate) fn resolve_sqlite_vacuum_timeout(configured: Option<&str>) -> Duration {
    let seconds = configured
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_SQLITE_VACUUM_TIMEOUT_SECONDS);
    Duration::from_secs(seconds)
}

fn sqlite_vacuum_timeout() -> Duration {
    resolve_sqlite_vacuum_timeout(std::env::var(SQLITE_VACUUM_TIMEOUT_ENV).ok().as_deref())
}

pub fn maintenance_enabled() -> bool {
    match std::env::var(MAINTENANCE_ENV) {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => true,
    }
}

/// Daily period in seconds; `0` disables the schedule. Invalid input falls back
/// to the default rather than disabling maintenance silently.
pub fn maintenance_interval_seconds() -> u64 {
    std::env::var(MAINTENANCE_INTERVAL_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAINTENANCE_INTERVAL_SECONDS)
}

pub fn maintenance_initial_delay_seconds() -> u64 {
    std::env::var(MAINTENANCE_INITIAL_DELAY_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAINTENANCE_INITIAL_DELAY_SECONDS)
}

impl Database {
    /// Collects maintenance candidates for the active backend.
    ///
    /// PostgreSQL reads `pg_stat_user_tables`; SQLite has no dead-tuple concept
    /// and instead reports what its freelist says about reclaimable pages.
    /// True when a scan or metadata job currently holds the database.
    async fn maintenance_scan_in_progress(&self) -> Result<bool, StorageError> {
        let active: i64 = self
            .query_scalar(
                "SELECT CASE WHEN EXISTS(
                    SELECT 1 FROM scan_jobs WHERE status IN ('PENDING', 'RUNNING')
                ) OR EXISTS(
                    SELECT 1 FROM metadata_reidentify_jobs WHERE status IN ('PENDING', 'RUNNING')
                ) THEN 1 ELSE 0 END",
            )
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(active != 0)
    }

    /// Whether a client is watching right now.
    ///
    /// A full `VACUUM` rewrites the file while holding the write lock, and playback progress is
    /// persisted every few seconds, so a round that starts during playback competes with the one
    /// write a user notices immediately -- the resume position of the film they are watching.
    async fn active_playback_in_progress(&self) -> Result<bool, StorageError> {
        let query = format!(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM playback_sessions
                 WHERE state IN ('PLAYING', 'PAUSED')
                   AND last_event_at > unixepoch() - {PLAYBACK_SESSION_STALE_AFTER_SECONDS}
             ) OR EXISTS(
                 SELECT 1 FROM web_playback_sessions
                 WHERE state = 'ACTIVE'
                   AND last_heartbeat_at > unixepoch() - {PLAYBACK_SESSION_STALE_AFTER_SECONDS}
                   AND expires_at >= unixepoch()
             ) THEN 1 ELSE 0 END"
        );
        let active: i64 = self
            .query_scalar(sqlx::AssertSqlSafe(query))
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(active != 0)
    }

    async fn maintenance_candidates(&self) -> Result<Vec<MaintenanceCandidate>, StorageError> {
        match self.backend {
            DatabaseBackend::Sqlite => self.sqlite_maintenance_candidates().await,
            DatabaseBackend::Postgres => self.postgres_maintenance_candidates().await,
        }
    }

    async fn postgres_maintenance_candidates(
        &self,
    ) -> Result<Vec<MaintenanceCandidate>, StorageError> {
        let rows = self
            .query(
                "SELECT relname::TEXT AS table_name,
                        n_live_tup::BIGINT AS live_tuples,
                        n_dead_tup::BIGINT AS dead_tuples,
                        autoanalyze_count::BIGINT AS analyze_count,
                        autovacuum_count::BIGINT AS vacuum_count
                 FROM pg_stat_user_tables
                 WHERE n_dead_tup > 0
                 ORDER BY n_dead_tup DESC
                 LIMIT 100",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let mut candidates = Vec::with_capacity(rows.len());
        for row in rows {
            let table: String = row.try_get("table_name").unwrap_or_default();
            let live: i64 = row.try_get("live_tuples").unwrap_or_default();
            let dead: i64 = row.try_get("dead_tuples").unwrap_or_default();
            let analyze_count: i64 = row.try_get("analyze_count").unwrap_or_default();
            let vacuum_count: i64 = row.try_get("vacuum_count").unwrap_or_default();
            // A table that never ran analyze, autoanalyze or vacuum carries the
            // pg_stat placeholder `0` for its live tuple count. Real deployments
            // have shown that value hiding hundreds of thousands of rows.
            let statistics_trusted = analyze_count > 0 || vacuum_count > 0;
            candidates.push(MaintenanceCandidate {
                table,
                dead_tuples: dead,
                live_tuples: live,
                statistics_trusted,
            });
        }
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.dead_tuples));
        Ok(candidates)
    }

    async fn sqlite_maintenance_candidates(
        &self,
    ) -> Result<Vec<MaintenanceCandidate>, StorageError> {
        let freelist: i64 = self
            .query_scalar("PRAGMA freelist_count")
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let page_count: i64 = self
            .query_scalar("PRAGMA page_count")
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        // SQLite tracks no per-table dead tuples. Model the database-wide
        // freelist as one synthetic candidate so the round can run `VACUUM` when
        // enough pages are reclaimable, and skip it otherwise.
        let candidate = MaintenanceCandidate {
            table: "sqlite-main".to_owned(),
            dead_tuples: freelist,
            live_tuples: page_count,
            statistics_trusted: freelist > 0 && page_count > 0,
        };
        Ok(candidate
            .sqlite_freelist_worth_vacuum()
            .then_some(candidate)
            .into_iter()
            .collect())
    }

    async fn sqlite_auto_vacuum_mode(&self) -> Result<i64, StorageError> {
        self.query_scalar("PRAGMA auto_vacuum")
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())
    }

    /// Returns free pages to the filesystem in bounded steps. Incremental vacuum only
    /// truncates what sits at the tail of the file, so it stops as soon as a step no
    /// longer shrinks the freelist instead of spinning.
    async fn run_sqlite_incremental_vacuum(
        &self,
        table: &str,
        report: &mut MaintenanceReport,
    ) -> bool {
        let deadline = Instant::now() + sqlite_vacuum_timeout();
        loop {
            let freelist = match self.sqlite_freelist_count().await {
                Ok(freelist) => freelist,
                Err(error) => {
                    tracing::warn!(%error, table, "sqlite incremental vacuum could not read the freelist");
                    report.failed.push(table.to_owned());
                    return false;
                }
            };
            if freelist <= 0 {
                report.vacuumed.push(table.to_owned());
                return true;
            }
            if Instant::now() >= deadline {
                tracing::warn!(
                    table,
                    freelist,
                    "sqlite incremental vacuum stopped at its time budget"
                );
                report.failed.push(table.to_owned());
                return false;
            }
            let statement =
                format!("PRAGMA incremental_vacuum({SQLITE_INCREMENTAL_VACUUM_PAGES_PER_STEP})");
            if let Err(error) = self
                .query(sqlx::AssertSqlSafe(statement))
                .execute(&self.pool)
                .await
            {
                tracing::warn!(%error, table, "sqlite incremental vacuum failed");
                report.failed.push(table.to_owned());
                return false;
            }
            let remaining = self.sqlite_freelist_count().await.unwrap_or(freelist);
            if remaining >= freelist {
                // Only live pages sit at the tail now; a full VACUUM would be needed to
                // move them, and that is an offline decision.
                report.vacuumed.push(table.to_owned());
                return true;
            }
        }
    }

    async fn sqlite_freelist_count(&self) -> Result<i64, StorageError> {
        self.query_scalar("PRAGMA freelist_count")
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())
    }

    async fn run_maintenance_vacuum(&self, table: &str, report: &mut MaintenanceReport) -> bool {
        match self.backend {
            DatabaseBackend::Sqlite => {
                if self.sqlite_auto_vacuum_mode().await.unwrap_or_default()
                    == SQLITE_AUTO_VACUUM_INCREMENTAL
                {
                    return self.run_sqlite_incremental_vacuum(table, report).await;
                }
                match tokio::time::timeout(
                    sqlite_vacuum_timeout(),
                    self.query("VACUUM").execute(&self.pool),
                )
                .await
                {
                    Ok(Ok(_)) => {
                        report.vacuumed.push(table.to_owned());
                        true
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, table, "sqlite vacuum failed");
                        report.failed.push(table.to_owned());
                        false
                    }
                    Err(_) => {
                        tracing::warn!(
                            table,
                            "sqlite vacuum timed out; this database predates incremental                              auto-vacuum, so reclaiming the space needs an offline VACUUM"
                        );
                        report.failed.push(table.to_owned());
                        false
                    }
                }
            }
            DatabaseBackend::Postgres => {
                // Parallel index vacuum needs shared memory beyond Docker's 64 MB
                // default /dev/shm. Keep maintenance single-process so it cannot
                // fail with "could not resize shared memory segment".
                let statement = format!("VACUUM (ANALYZE, PARALLEL 0) {}", quote_identifier(table));
                match tokio::time::timeout(
                    PER_TABLE_TIMEOUT,
                    self.query(sqlx::AssertSqlSafe(statement))
                        .execute(&self.pool),
                )
                .await
                {
                    Ok(Ok(_)) => {
                        report.vacuumed.push(table.to_owned());
                        true
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(%error, table, "postgres vacuum failed");
                        report.failed.push(table.to_owned());
                        false
                    }
                    Err(_) => {
                        tracing::warn!(table, "postgres vacuum timed out");
                        report.failed.push(table.to_owned());
                        false
                    }
                }
            }
        }
    }

    /// Applies table-level autovacuum overrides. SQLite has no autovacuum, so the
    /// caller-facing report keeps it out of `parameters_applied`.
    async fn apply_maintenance_parameters(&self, table: &str) -> Result<(), StorageError> {
        if self.backend == DatabaseBackend::Sqlite {
            return Ok(());
        }
        let statement = format!(
            "ALTER TABLE {} SET (autovacuum_vacuum_scale_factor = 0.02, autovacuum_vacuum_threshold = 5000, autovacuum_analyze_scale_factor = 0.02, autovacuum_analyze_threshold = 5000)",
            quote_identifier(table)
        );
        self.query(sqlx::AssertSqlSafe(statement))
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(self.sqlx_err())
    }
}

/// Statistics refresh outcomes. Every skip is a deliberate decision, not a failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqliteStatisticsRefresh {
    /// Statistics existed already; the planner was told to check whether they still fit.
    Optimized,
    /// The database had never been analyzed, so a full `ANALYZE` ran.
    Analyzed,
    /// No catalog rows yet. Statistics taken on an empty database teach the planner that every
    /// table has a single row, which it then plans full scans for long after the library filled up.
    SkippedEmpty,
    /// A scan or metadata job owns the database; describing it mid-flight would sample a shape
    /// that is about to change.
    SkippedBusy,
    /// PostgreSQL maintains its own statistics through autovacuum and the per-table
    /// `VACUUM (ANALYZE)` this module already issues.
    SkippedBackend,
}

/// `ANALYZE` reads every indexed row. A library that needs much longer than this is better served
/// by a partial sample than by holding the write lock for minutes.
const SQLITE_ANALYZE_TIMEOUT: Duration = Duration::from_secs(120);
/// Statistics refresh cadence. `PRAGMA optimize` is usually a no-op, so this stays cheap enough
/// to run far more often than the daily maintenance round that reclaims space.
const STATISTICS_REFRESH_INTERVAL: Duration = Duration::from_secs(900);
const STATISTICS_REFRESH_INITIAL_DELAY: Duration = Duration::from_secs(10);

impl Database {
    /// Keeps `sqlite_stat1` describing the catalog the planner actually sees.
    ///
    /// Without statistics SQLite falls back to fixed selectivity guesses, which is exactly the
    /// wrong input for the composite indexes the catalog relies on (`library_id` + `removed_at` +
    /// `has_available_source` and friends are anything but uniform).
    ///
    /// The first pass after a library appears runs a full `ANALYZE`; later passes only run
    /// `PRAGMA optimize`, which decides for itself whether any table grew enough to be worth
    /// re-sampling. The `analysis_limit` a pooled connection may have inherited from a previous
    /// `PRAGMA optimize` is restored after the full pass, so this never turns into sampled
    /// statistics by accident.
    pub async fn refresh_query_statistics(&self) -> Result<SqliteStatisticsRefresh, StorageError> {
        if self.backend != DatabaseBackend::Sqlite {
            return Ok(SqliteStatisticsRefresh::SkippedBackend);
        }
        let _refresh_guard =
            acquire_write_lock(&self.statistics_refresh_lock, "sqlite_statistics").await;
        if self.maintenance_scan_in_progress().await? {
            return Ok(SqliteStatisticsRefresh::SkippedBusy);
        }
        let has_items: i64 = self
            .query_scalar("SELECT EXISTS(SELECT 1 FROM media_items)")
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        if has_items == 0 {
            return Ok(SqliteStatisticsRefresh::SkippedEmpty);
        }
        let analyzed: i64 = self
            .query_scalar(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_stat1')",
            )
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        if analyzed == 0 {
            self.analyze_catalog().await?;
            return Ok(SqliteStatisticsRefresh::Analyzed);
        }
        self.query("PRAGMA optimize")
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(SqliteStatisticsRefresh::Optimized)
    }

    /// Re-describes the catalog after a bulk change such as a completed scan or import.
    ///
    /// The periodic pass only runs `PRAGMA optimize`, which can leave a freshly scanned 60k-item
    /// library without statistics until the next 15-minute tick. The planner then falls back to
    /// fixed selectivity guesses and some catalog queries degrade by orders of magnitude
    /// (measured: a per-library "latest" selection taking 14 s instead of 2 ms). This pass is
    /// bounded by the same timeout and skips while a scan still owns the database.
    pub async fn refresh_query_statistics_after_bulk_change(
        &self,
    ) -> Result<SqliteStatisticsRefresh, StorageError> {
        if self.backend != DatabaseBackend::Sqlite {
            return Ok(SqliteStatisticsRefresh::SkippedBackend);
        }
        let _refresh_guard =
            acquire_write_lock(&self.statistics_refresh_lock, "sqlite_statistics").await;
        if self.maintenance_scan_in_progress().await? {
            return Ok(SqliteStatisticsRefresh::SkippedBusy);
        }
        let has_items: i64 = self
            .query_scalar("SELECT EXISTS(SELECT 1 FROM media_items)")
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        if has_items == 0 {
            return Ok(SqliteStatisticsRefresh::SkippedEmpty);
        }
        self.analyze_catalog().await?;
        Ok(SqliteStatisticsRefresh::Analyzed)
    }

    /// Runs [`Self::refresh_query_statistics_after_bulk_change`] in the background after a scan.
    pub fn spawn_bulk_change_statistics_refresh(&self) {
        if !maintenance_enabled() {
            return;
        }
        let database = self.clone();
        tokio::spawn(async move {
            match database.refresh_query_statistics_after_bulk_change().await {
                Ok(outcome) => {
                    tracing::debug!(?outcome, "post-scan query statistics refresh finished");
                }
                Err(error) => {
                    tracing::warn!(%error, "post-scan query statistics refresh failed");
                }
            }
        });
    }

    async fn analyze_catalog(&self) -> Result<(), StorageError> {
        let previous: Option<i64> = self
            .query_scalar("PRAGMA analysis_limit")
            .fetch_optional(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        self.query("PRAGMA analysis_limit = 0")
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let analyzed = tokio::time::timeout(
            SQLITE_ANALYZE_TIMEOUT,
            self.query("ANALYZE").execute(&self.pool),
        )
        .await;
        if let Some(limit) = previous {
            // The connection goes back to the pool, so hand it over the way it was handed to us.
            let restore = format!("PRAGMA analysis_limit = {limit}");
            let _ = self
                .query(sqlx::AssertSqlSafe(restore))
                .execute(&self.pool)
                .await;
        }
        match analyzed {
            Ok(Ok(_)) => {
                tracing::info!("analyzed sqlite catalog statistics for the first time");
                Ok(())
            }
            Ok(Err(source)) => Err(StorageError::Sqlx {
                path: self.path.clone(),
                source,
            }),
            Err(_) => {
                tracing::warn!(
                    timeout_seconds = SQLITE_ANALYZE_TIMEOUT.as_secs(),
                    "sqlite ANALYZE did not finish in time; keeping the statistics it wrote"
                );
                Ok(())
            }
        }
    }

    /// Keeps query statistics fresh between maintenance rounds.
    ///
    /// Separate from the space-reclaiming round on purpose: that one runs daily and skips whole
    /// scans, while a fresh library needs statistics within minutes of its first scan.
    pub fn spawn_query_statistics_refresh(&self) {
        if !maintenance_enabled() {
            tracing::info!("query statistics refresh disabled with database maintenance");
            return;
        }
        let database = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(STATISTICS_REFRESH_INITIAL_DELAY).await;
            let mut ticker = tokio::time::interval(STATISTICS_REFRESH_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                if let Err(error) = database.refresh_query_statistics().await {
                    tracing::warn!(%error, "refreshing query statistics failed");
                }
            }
        });
    }
}

/// Quotes a table name for interpolation into DDL that cannot take bind
/// parameters. Names come from system catalogs, never user input.
/// `LUX_POSTGRES_QUERY_STATISTICS_EXTENSION` defaults to on; `0`, `false`, `off` and `no` opt out.
pub(crate) fn resolve_query_statistics_extension_enabled(configured: Option<&str>) -> bool {
    !matches!(
        configured
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref(),
        Some("0" | "false" | "off" | "no")
    )
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum QueryStatisticsExtensionPlan {
    Disabled,
    /// Creating the extension without the preload would only make every query fail.
    NotPreloaded,
    AlreadyInstalled,
    Create,
}

pub(crate) fn plan_query_statistics_extension(
    enabled: bool,
    preload_libraries: &str,
    installed: bool,
) -> QueryStatisticsExtensionPlan {
    if !enabled {
        QueryStatisticsExtensionPlan::Disabled
    } else if installed {
        QueryStatisticsExtensionPlan::AlreadyInstalled
    } else if !super::database_diagnostics::shared_preload_contains_pg_stat_statements(
        preload_libraries,
    ) {
        QueryStatisticsExtensionPlan::NotPreloaded
    } else {
        QueryStatisticsExtensionPlan::Create
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum QueryStatisticsExtensionOutcome {
    NotApplicable,
    Disabled,
    NotPreloaded,
    AlreadyInstalled,
    Created,
    Failed,
}

impl Database {
    /// Installs `pg_stat_statements` in the business database when the server already preloads
    /// it, so the administrator diagnostics can report query statistics without a manual step.
    ///
    /// Best effort: the extension needs a superuser on most servers, so a managed or external
    /// PostgreSQL may refuse. A refusal is logged once and never affects startup.
    pub(crate) async fn ensure_postgres_query_statistics_extension(
        &self,
    ) -> QueryStatisticsExtensionOutcome {
        if self.backend != DatabaseBackend::Postgres {
            return QueryStatisticsExtensionOutcome::NotApplicable;
        }
        let enabled = resolve_query_statistics_extension_enabled(
            std::env::var(QUERY_STATISTICS_EXTENSION_ENV)
                .ok()
                .as_deref(),
        );
        let state = match self
            .query_as::<(String, bool)>(
                "SELECT current_setting('shared_preload_libraries')::TEXT,
                        EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements')",
            )
            .fetch_one(&self.pool)
            .await
        {
            Ok(state) => state,
            Err(error) => {
                tracing::warn!(%error, "could not inspect the pg_stat_statements configuration");
                return QueryStatisticsExtensionOutcome::Failed;
            }
        };
        match plan_query_statistics_extension(enabled, &state.0, state.1) {
            QueryStatisticsExtensionPlan::Disabled => QueryStatisticsExtensionOutcome::Disabled,
            QueryStatisticsExtensionPlan::NotPreloaded => {
                tracing::info!(
                    "pg_stat_statements is not in shared_preload_libraries; query statistics stay unavailable"
                );
                QueryStatisticsExtensionOutcome::NotPreloaded
            }
            QueryStatisticsExtensionPlan::AlreadyInstalled => {
                QueryStatisticsExtensionOutcome::AlreadyInstalled
            }
            QueryStatisticsExtensionPlan::Create => {
                match tokio::time::timeout(
                    QUERY_STATISTICS_EXTENSION_TIMEOUT,
                    self.query("CREATE EXTENSION IF NOT EXISTS pg_stat_statements")
                        .execute(&self.pool),
                )
                .await
                {
                    Ok(Ok(_)) => {
                        tracing::info!("created the pg_stat_statements extension");
                        QueryStatisticsExtensionOutcome::Created
                    }
                    Ok(Err(error)) => {
                        tracing::warn!(
                            %error,
                            "could not create pg_stat_statements; ask a database superuser to run CREATE EXTENSION pg_stat_statements"
                        );
                        QueryStatisticsExtensionOutcome::Failed
                    }
                    Err(_) => {
                        tracing::warn!("creating pg_stat_statements timed out");
                        QueryStatisticsExtensionOutcome::Failed
                    }
                }
            }
        }
    }

    /// Runs [`Self::ensure_postgres_query_statistics_extension`] once in the background, independent
    /// of the periodic maintenance schedule.
    pub fn spawn_postgres_query_statistics_extension(&self) {
        let database = self.clone();
        tokio::spawn(async move {
            database.ensure_postgres_query_statistics_extension().await;
        });
    }
}

fn quote_identifier(name: &str) -> String {
    let escaped = name.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

impl Database {
    /// Starts the delayed, daily maintenance loop. No-op when disabled so
    /// operators can turn it off without a restart of dependent services.
    pub fn spawn_database_maintenance(&self) {
        if !maintenance_enabled() {
            tracing::info!("database maintenance disabled by configuration");
            return;
        }
        let interval_seconds = maintenance_interval_seconds();
        if interval_seconds == 0 {
            tracing::info!("database maintenance schedule disabled");
            return;
        }
        let database = self.clone();
        tokio::spawn(async move {
            let initial_delay = Duration::from_secs(maintenance_initial_delay_seconds());
            tokio::time::sleep(initial_delay).await;
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_seconds));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                let report = database.run_database_maintenance_round().await;
                if report.vacuumed.is_empty()
                    && report.parameters_applied.is_empty()
                    && report.failed.is_empty()
                {
                    continue;
                }
                tracing::info!(
                    vacuumed = report.vacuumed.len(),
                    parameters_applied = report.parameters_applied.len(),
                    skipped_untrusted = report.skipped_untrusted.len(),
                    skipped_below_threshold = report.skipped_below_threshold.len(),
                    failed = report.failed.len(),
                    "database maintenance round finished"
                );
            }
        });
    }

    /// Runs one bounded maintenance round. Refuses to start when another round
    /// is in flight, and defers to scanning work so the maintenance pass never
    /// competes with an active scan.
    pub async fn run_database_maintenance_round(&self) -> MaintenanceReport {
        if MAINTENANCE_RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
            tracing::debug!("database maintenance round already running");
            return MaintenanceReport::default();
        }
        let report = self.perform_maintenance_round().await;
        MAINTENANCE_RUNNING.store(false, std::sync::atomic::Ordering::Release);
        report
    }

    async fn perform_maintenance_round(&self) -> MaintenanceReport {
        let mut report = MaintenanceReport::default();
        match self.maintenance_scan_in_progress().await {
            Ok(true) => {
                // Scanning owns the database while a job runs. Skipping this round
                // is cheaper than competing with the scan for I/O and locks.
                tracing::debug!("database maintenance skipped while a scan is running");
                return report;
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, "database maintenance scan check failed");
                return report;
            }
        }
        match self.active_playback_in_progress().await {
            Ok(true) => {
                // Deferring costs nothing: the next round reclaims the same pages, and the client
                // never sees its progress write fail against a VACUUM holding the write lock.
                tracing::debug!("database maintenance deferred while a client is streaming");
                return report;
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, "database maintenance playback check failed");
                return report;
            }
        }
        if self.backend == DatabaseBackend::Postgres {
            // media_search is populated by triggers and can remain "untrusted"
            // forever because autovacuum has never analyzed it. Refresh its
            // statistics before selecting maintenance candidates.
            match tokio::time::timeout(
                Duration::from_secs(30),
                self.query("ANALYZE media_search").execute(&self.pool),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "media_search analyze failed");
                }
                Err(_) => tracing::warn!("media_search analyze timed out"),
            }
        }
        let candidates = match self.maintenance_candidates().await {
            Ok(candidates) => candidates,
            Err(error) => {
                tracing::warn!(%error, "database maintenance candidate query failed");
                return report;
            }
        };
        for candidate in candidates.into_iter().take(MAX_MAINTENANCE_TABLES) {
            if !candidate.can_be_evaluated() {
                report.skipped_untrusted.push(candidate.table.clone());
                continue;
            }
            let worth_vacuum = match self.backend {
                DatabaseBackend::Sqlite => candidate.sqlite_freelist_worth_vacuum(),
                DatabaseBackend::Postgres => candidate.exceeds_vacuum_trigger(),
            };
            if !worth_vacuum {
                report.skipped_below_threshold.push(candidate.table.clone());
                continue;
            }
            if !self
                .run_maintenance_vacuum(&candidate.table, &mut report)
                .await
            {
                continue;
            }
            if candidate.deserves_parameter_override() {
                if let Err(error) = self.apply_maintenance_parameters(&candidate.table).await {
                    tracing::warn!(%error, table = %candidate.table, "database maintenance parameters failed");
                } else {
                    report.parameters_applied.push(candidate.table.clone());
                }
            }
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(live: i64, dead: i64, trusted: bool) -> MaintenanceCandidate {
        MaintenanceCandidate {
            table: "t".into(),
            dead_tuples: dead,
            live_tuples: live,
            statistics_trusted: trusted,
        }
    }

    #[test]
    fn untrusted_statistics_are_never_evaluated() {
        // A table whose autovacuum never ran reports live=0 exactly like a truly
        // empty table; the guard must keep both out of the action path.
        let report = candidate(0, 0, false);
        assert!(!report.can_be_evaluated());
        assert!(!report.exceeds_vacuum_trigger());
        assert!(!report.deserves_parameter_override());
    }

    #[test]
    fn dead_tuple_floor_blocks_small_tables() {
        let small = candidate(1_000, 4_999, true);
        assert!(!small.exceeds_vacuum_trigger());
    }

    #[test]
    fn trigger_scales_with_live_tuples() {
        // 0.2 * 1_000_000 + 50 = 200_050 trigger; 70% of it is 140_035.
        let below = candidate(1_000_000, 140_000, true);
        assert!(!below.exceeds_vacuum_trigger());
        let above = candidate(1_000_000, 140_100, true);
        assert!(above.exceeds_vacuum_trigger());
    }

    #[test]
    fn parameter_override_requires_large_table() {
        let small = candidate(500, 10_000, true);
        assert!(small.exceeds_vacuum_trigger());
        assert!(!small.deserves_parameter_override());
        let large = candidate(50_000, 10_000, true);
        assert!(large.deserves_parameter_override());
    }

    #[test]
    fn sqlite_vacuum_requires_absolute_and_relative_freelist() {
        let below_absolute = candidate(10_000, 255, true);
        assert!(!below_absolute.sqlite_freelist_worth_vacuum());
        let below_ratio = candidate(100_000, 4_999, true);
        assert!(!below_ratio.sqlite_freelist_worth_vacuum());
        let worth = candidate(10_000, 1_000, true);
        assert!(worth.sqlite_freelist_worth_vacuum());
        let untrusted = candidate(10_000, 1_000, false);
        assert!(!untrusted.sqlite_freelist_worth_vacuum());
        let empty = candidate(0, 1_000, true);
        assert!(!empty.sqlite_freelist_worth_vacuum());
    }

    #[test]
    fn defaults_keep_the_schedule_enabled_and_delayed() {
        // The schedule must default to a positive period and a positive first
        // delay, so startup work owns the first minutes of a deployment.
        assert_ne!(DEFAULT_MAINTENANCE_INTERVAL_SECONDS, 0);
        assert_ne!(DEFAULT_MAINTENANCE_INITIAL_DELAY_SECONDS, 0);
        let interval = DEFAULT_MAINTENANCE_INTERVAL_SECONDS;
        let delay = DEFAULT_MAINTENANCE_INITIAL_DELAY_SECONDS;
        assert!(
            delay < interval,
            "first round must precede the daily cadence"
        );
    }

    #[test]
    fn query_statistics_extension_is_created_only_when_preloaded_and_missing() {
        use super::{QueryStatisticsExtensionPlan as Plan, plan_query_statistics_extension};
        assert_eq!(
            plan_query_statistics_extension(true, "pg_stat_statements", false),
            Plan::Create
        );
        assert_eq!(
            plan_query_statistics_extension(true, "auto_explain, \"pg_stat_statements\"", false),
            Plan::Create
        );
        assert_eq!(
            plan_query_statistics_extension(true, "pg_stat_statements", true),
            Plan::AlreadyInstalled
        );
        // Without the preload the extension would exist but every query would fail.
        assert_eq!(
            plan_query_statistics_extension(true, "", false),
            Plan::NotPreloaded
        );
        assert_eq!(
            plan_query_statistics_extension(true, "auto_explain", false),
            Plan::NotPreloaded
        );
        assert_eq!(
            plan_query_statistics_extension(false, "pg_stat_statements", false),
            Plan::Disabled
        );
    }

    #[test]
    fn query_statistics_extension_switch_defaults_on() {
        use super::resolve_query_statistics_extension_enabled as resolve;
        assert!(resolve(None));
        assert!(resolve(Some("1")));
        assert!(resolve(Some("yes please")));
        for off in ["0", "false", "OFF", " no "] {
            assert!(!resolve(Some(off)), "{off:?} must disable the extension");
        }
    }
}
