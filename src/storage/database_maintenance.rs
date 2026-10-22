use super::*;
use std::time::Duration;

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

const MAINTENANCE_ENV: &str = "LUX_DATABASE_MAINTENANCE";
const MAINTENANCE_INTERVAL_ENV: &str = "LUX_DATABASE_MAINTENANCE_INTERVAL_SECONDS";
const MAINTENANCE_INITIAL_DELAY_ENV: &str = "LUX_DATABASE_MAINTENANCE_INITIAL_DELAY_SECONDS";

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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let page_count: i64 = self
            .query_scalar("PRAGMA page_count")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
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

    async fn run_maintenance_vacuum(&self, table: &str, report: &mut MaintenanceReport) -> bool {
        match self.backend {
            DatabaseBackend::Sqlite => match tokio::time::timeout(
                PER_TABLE_TIMEOUT,
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
                    tracing::warn!(table, "sqlite vacuum timed out");
                    report.failed.push(table.to_owned());
                    false
                }
            },
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }
}

/// Quotes a table name for interpolation into DDL that cannot take bind
/// parameters. Names come from system catalogs, never user input.
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
}
