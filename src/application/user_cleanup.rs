use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    application::{
        background_loop::{BackgroundLoop, wait_or_shutdown},
        user_avatars::UserAvatarService,
    },
    domain::ids::UserId,
    storage::{Database, StorageError},
};

/// Every retention this worker enforces is measured in days, so an hourly sweep already wins by
/// three orders of magnitude. The previous one-minute period woke an idle server 1 440 times a day
/// and ran three queries per wake to find nothing to purge.
pub(crate) const USER_CLEANUP_INTERVAL: Duration = Duration::from_secs(3_600);
/// The first sweep still runs shortly after startup instead of an hour later: a restart must not
/// postpone retention work that was already due.
pub(crate) const USER_CLEANUP_FIRST_DELAY: Duration = Duration::from_secs(60);
/// One pass drains an arbitrary backlog (the batches are small), so a server that was off for a
/// month does not need a month of hourly passes to catch up. The cap keeps a pathological backlog
/// from holding the pass - and the shutdown signal - for an unbounded time.
const USER_CLEANUP_MAX_BATCHES_PER_PASS: usize = 200;
const USER_CLEANUP_BATCH_SIZE: i64 = 50;
const USER_ACTIVITY_RETENTION_BATCH_SIZE: i64 = 1_000;
const SECONDS_PER_DAY: i64 = 86_400;

/// True when a bounded delete filled its batch, i.e. more rows are waiting for the next one.
fn batch_was_full(deleted: u64, batch_size: i64) -> bool {
    deleted >= u64::try_from(batch_size.max(1)).unwrap_or(u64::MAX)
}

async fn purge_expired_user_activity_batch(
    database: &Database,
    now: i64,
) -> Result<u64, StorageError> {
    let retention_days = database.user_activity_retention_days().await?;
    let retention_seconds = retention_days.saturating_mul(SECONDS_PER_DAY);
    let occurred_before = now.saturating_sub(retention_seconds);
    database
        .delete_user_activity_events_before(occurred_before, USER_ACTIVITY_RETENTION_BATCH_SIZE)
        .await
}

/// Owns the retention loop so a shutdown can stop it before the database closes.
///
/// The loop used to be a bare `tokio::spawn` whose handle was dropped: graceful shutdown could not
/// drain a purge that was already deleting rows. The worker is shared through the service table, so
/// shutting it down is a method call on the same instance every clone sees.
#[derive(Clone)]
pub(crate) struct UserCleanupWorker {
    worker: Arc<tokio::sync::Mutex<Option<BackgroundLoop>>>,
}

impl UserCleanupWorker {
    pub(crate) fn start(
        database: Database,
        avatars: UserAvatarService,
        config_dir: Option<PathBuf>,
    ) -> Self {
        Self::start_with_timings(
            database,
            avatars,
            config_dir,
            USER_CLEANUP_FIRST_DELAY,
            USER_CLEANUP_INTERVAL,
        )
    }

    /// Starts with explicit timings; tests use a short delay so a pass runs immediately.
    pub(crate) fn start_with_timings(
        database: Database,
        avatars: UserAvatarService,
        config_dir: Option<PathBuf>,
        first_delay: Duration,
        interval: Duration,
    ) -> Self {
        let worker = match tokio::runtime::Handle::try_current() {
            Ok(_) => Some(BackgroundLoop::spawn("user_cleanup", move |shutdown| {
                let database = database.clone();
                let avatars = avatars.clone();
                let config_dir = config_dir.clone();
                async move {
                    run_user_cleanup_worker(
                        database,
                        avatars,
                        config_dir,
                        first_delay,
                        interval,
                        shutdown,
                    )
                    .await
                }
            })),
            // Without a runtime (checkout-style construction in tests) there is nothing to stop.
            Err(_) => None,
        };
        Self {
            worker: Arc::new(tokio::sync::Mutex::new(worker)),
        }
    }

    pub(crate) async fn shutdown(&self) {
        let worker = self.worker.lock().await.take();
        if let Some(worker) = worker {
            worker.shutdown().await;
        }
    }
}

async fn run_user_cleanup_worker(
    database: Database,
    avatars: UserAvatarService,
    config_dir: Option<PathBuf>,
    first_delay: Duration,
    interval: Duration,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    if wait_or_shutdown(&mut shutdown, first_delay).await {
        return;
    }
    loop {
        run_user_cleanup_pass(&database, &avatars, config_dir.as_deref()).await;
        if wait_or_shutdown(&mut shutdown, interval).await {
            return;
        }
    }
}

async fn run_user_cleanup_pass(
    database: &Database,
    avatars: &UserAvatarService,
    config_dir: Option<&std::path::Path>,
) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64;
    drain_expired_users(database, avatars, now).await;
    drain_expired_user_activity(database, now).await;
    drain_expired_session_history(database, now).await;
    drain_expired_emby_direct_playback_tickets(database, now).await;
    drain_expired_account_records(database, now).await;
    drain_expired_playback_history(config_dir, database, now).await;
}

/// Lets other tasks (and the shutdown signal) run between batches of one pass.
async fn yield_between_batches() {
    tokio::task::yield_now().await;
}

async fn drain_expired_users(database: &Database, avatars: &UserAvatarService, now: i64) {
    for _ in 0..USER_CLEANUP_MAX_BATCHES_PER_PASS {
        let deleted_user_ids = match database
            .purge_expired_deleted_users(now, USER_CLEANUP_BATCH_SIZE)
            .await
        {
            Ok(user_ids) => user_ids,
            Err(error) => {
                tracing::warn!(%error, "failed to purge expired user accounts");
                return;
            }
        };
        let filled = batch_was_full(
            u64::try_from(deleted_user_ids.len()).unwrap_or(u64::MAX),
            USER_CLEANUP_BATCH_SIZE,
        );
        for user_id in deleted_user_ids {
            let Ok(user_id) = user_id.parse::<UserId>() else {
                tracing::warn!("purged user has an invalid ID; avatar cleanup was skipped");
                continue;
            };
            if let Err(error) = avatars.remove(user_id).await {
                tracing::warn!(%error, "failed to remove avatar for a purged user");
            }
        }
        if !filled {
            return;
        }
        yield_between_batches().await;
    }
}

async fn drain_expired_user_activity(database: &Database, now: i64) {
    for _ in 0..USER_CLEANUP_MAX_BATCHES_PER_PASS {
        match purge_expired_user_activity_batch(database, now).await {
            Ok(deleted) => {
                if !batch_was_full(deleted, USER_ACTIVITY_RETENTION_BATCH_SIZE) {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to purge expired user activity events");
                return;
            }
        }
        yield_between_batches().await;
    }
}

async fn drain_expired_session_history(database: &Database, now: i64) {
    for _ in 0..USER_CLEANUP_MAX_BATCHES_PER_PASS {
        match database
            .purge_expired_session_history(now, USER_CLEANUP_BATCH_SIZE)
            .await
        {
            Ok(purge) => {
                let filled = [
                    purge.web_sessions,
                    purge.access_tokens,
                    purge.web_playback_sessions,
                    purge.playback_sessions,
                    purge.playback_admissions,
                ]
                .into_iter()
                .any(|deleted| batch_was_full(deleted, USER_CLEANUP_BATCH_SIZE));
                if !filled {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to purge expired session and playback history");
                return;
            }
        }
        yield_between_batches().await;
    }
}

async fn drain_expired_emby_direct_playback_tickets(database: &Database, now: i64) {
    for _ in 0..USER_CLEANUP_MAX_BATCHES_PER_PASS {
        match database
            .delete_expired_emby_direct_playback_tickets(now, USER_CLEANUP_BATCH_SIZE)
            .await
        {
            Ok(deleted) => {
                if !batch_was_full(deleted, USER_CLEANUP_BATCH_SIZE) {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to purge expired Emby direct-play tickets");
                return;
            }
        }
        yield_between_batches().await;
    }
}

async fn drain_expired_account_records(database: &Database, now: i64) {
    for _ in 0..USER_CLEANUP_MAX_BATCHES_PER_PASS {
        match database
            .purge_expired_account_records(now, USER_CLEANUP_BATCH_SIZE)
            .await
        {
            Ok(purge) => {
                let filled = [
                    purge.user_invitations,
                    purge.mail_outbox,
                    purge.mail_challenges,
                    purge.user_devices,
                    purge.media_merge_records,
                    purge.user_registration_requests,
                    purge.emby_migration_jobs,
                    purge.metadata_candidates,
                ]
                .into_iter()
                .any(|deleted| batch_was_full(deleted, USER_CLEANUP_BATCH_SIZE));
                if !filled {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to purge expired account records");
                return;
            }
        }
        yield_between_batches().await;
    }
}

async fn drain_expired_playback_history(
    config_dir: Option<&std::path::Path>,
    database: &Database,
    now: i64,
) {
    let Some(config_dir) = config_dir else {
        return;
    };
    for _ in 0..USER_CLEANUP_MAX_BATCHES_PER_PASS {
        match crate::application::playback_history_retention::archive_playback_history(
            config_dir,
            database,
            now,
            USER_CLEANUP_BATCH_SIZE,
        )
        .await
        {
            Ok(archived) => {
                if !batch_was_full(archived, USER_CLEANUP_BATCH_SIZE) {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to archive expired playback history");
                return;
            }
        }
        yield_between_batches().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, storage::Database};

    #[tokio::test]
    async fn configured_activity_retention_prunes_one_bounded_batch_and_keeps_boundary()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        database.set_user_activity_retention_days(1).await?;

        let now = 3 * SECONDS_PER_DAY;
        let cutoff = now - SECONDS_PER_DAY;
        let mut transaction = database.pool().begin().await?;
        for index in 0..=USER_ACTIVITY_RETENTION_BATCH_SIZE {
            sqlx::query(
                "INSERT INTO user_activity_events (id, user_id, event_type, occurred_at)
                 VALUES (?, 'retention-worker-user', 'AUTH_LOGIN', ?)",
            )
            .bind(format!("retention-worker-old-{index}"))
            .bind(cutoff - 1)
            .execute(&mut *transaction)
            .await?;
        }
        sqlx::query(
            "INSERT INTO user_activity_events (id, user_id, event_type, occurred_at)
             VALUES ('retention-worker-boundary', 'retention-worker-user', 'AUTH_LOGIN', ?)",
        )
        .bind(cutoff)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;

        assert_eq!(
            purge_expired_user_activity_batch(&database, now).await?,
            1_000
        );
        let remaining_old: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user_activity_events WHERE occurred_at < ?")
                .bind(cutoff)
                .fetch_one(database.pool())
                .await?;
        assert_eq!(remaining_old, 1);
        assert_eq!(purge_expired_user_activity_batch(&database, now).await?, 1);
        let boundary_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_activity_events WHERE id = 'retention-worker-boundary'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(boundary_count, 1);
        Ok(())
    }

    /// Builds a database, one expired activity row and the worker dependencies.
    async fn worker_fixture() -> Result<
        (
            tempfile::TempDir,
            Database,
            UserAvatarService,
            Option<PathBuf>,
        ),
        Box<dyn std::error::Error>,
    > {
        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        database.set_user_activity_retention_days(1).await?;
        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        sqlx::query(
            "INSERT INTO user_activity_events (id, user_id, event_type, occurred_at)
             VALUES ('worker-expired-event', 'retention-worker-user', 'AUTH_LOGIN', ?)",
        )
        .bind(now - 2 * SECONDS_PER_DAY)
        .execute(database.pool())
        .await?;
        let avatars = UserAvatarService::new(temp_dir.path().join("config"));
        Ok((temp_dir, database, avatars, None))
    }

    async fn expired_event_count(database: &Database) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_activity_events WHERE id = 'worker-expired-event'",
        )
        .fetch_one(database.pool())
        .await
    }

    /// The production cadence is "one short delay after startup, then hourly".
    #[test]
    fn the_first_sweep_runs_soon_and_later_sweeps_are_hourly() {
        assert_eq!(USER_CLEANUP_FIRST_DELAY, Duration::from_secs(60));
        assert_eq!(USER_CLEANUP_INTERVAL, Duration::from_secs(3_600));
    }

    /// One pass must drain a backlog larger than a single batch: with the old single-batch pass the
    /// hourly cadence would clear 50 rows an hour.
    #[tokio::test]
    async fn one_pass_clears_a_backlog_larger_than_one_batch()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database, avatars, _config_dir) = worker_fixture().await?;
        let backlog = 120_usize;
        assert!(
            backlog > usize::try_from(USER_CLEANUP_BATCH_SIZE).unwrap_or(50),
            "the fixture must exceed one batch"
        );
        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        for index in 0..backlog {
            let user_id = format!("backlog-user-{index}");
            database
                .insert_user(&user_id, &user_id, "Backlog User", "hash", false, true)
                .await?;
            // Both markers are already due, so every row is purgeable in this pass.
            sqlx::query("UPDATE users SET deleted_at = ?, deletion_purge_at = 0 WHERE id = ?")
                .bind(now - SECONDS_PER_DAY)
                .bind(&user_id)
                .execute(database.pool())
                .await?;
        }

        run_user_cleanup_pass(&database, &avatars, None).await;

        let remaining: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE deleted_at IS NOT NULL")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(
            remaining, 0,
            "a single pass must drain the whole backlog, not one batch"
        );
        Ok(())
    }

    /// The first sweep must not wait a full hour after startup.
    #[tokio::test]
    async fn the_first_sweep_runs_after_the_short_startup_delay()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database, avatars, config_dir) = worker_fixture().await?;
        let worker = UserCleanupWorker::start_with_timings(
            database.clone(),
            avatars,
            config_dir,
            Duration::from_millis(20),
            Duration::from_secs(3_600),
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if expired_event_count(&database).await? == 0 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the first sweep must run after the short startup delay, not the hourly interval"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::timeout(Duration::from_secs(1), worker.shutdown())
            .await
            .expect("shutdown must stop the worker");
        Ok(())
    }

    /// The worker must stay asleep until its period elapses: an idle server used to wake it every
    /// minute, and one iteration must not happen before the (now hourly) interval.
    #[tokio::test]
    async fn worker_does_not_sweep_before_the_interval() -> Result<(), Box<dyn std::error::Error>> {
        assert!(
            USER_CLEANUP_INTERVAL >= Duration::from_secs(3_600),
            "the idle period must stay hourly or longer"
        );
        let (_temp_dir, database, avatars, config_dir) = worker_fixture().await?;
        let worker = UserCleanupWorker::start(database.clone(), avatars, config_dir);

        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(
            expired_event_count(&database).await?,
            1,
            "an idle worker must not run a retention pass before its interval"
        );
        tokio::time::timeout(Duration::from_secs(1), worker.shutdown())
            .await
            .expect("shutdown must not wait for the next interval");
        Ok(())
    }

    /// The loop still does its job once the interval elapses.
    #[tokio::test]
    async fn worker_sweeps_retention_once_the_interval_elapses()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database, avatars, config_dir) = worker_fixture().await?;
        let worker = UserCleanupWorker::start_with_timings(
            database.clone(),
            avatars,
            config_dir,
            Duration::from_millis(20),
            Duration::from_millis(50),
        );
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            if expired_event_count(&database).await? == 0 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the worker must purge expired activity once its interval elapses"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tokio::time::timeout(Duration::from_secs(1), worker.shutdown())
            .await
            .expect("shutdown must stop a worker that already ran");
        Ok(())
    }

    /// Shutdown must return while the loop is sleeping through the hourly interval.
    #[tokio::test]
    async fn shutdown_stops_the_worker_within_the_idle_interval() {
        let (_temp_dir, database, avatars, config_dir) =
            worker_fixture().await.expect("worker fixture");
        let worker = UserCleanupWorker::start(database, avatars, config_dir);
        tokio::time::timeout(Duration::from_secs(1), worker.shutdown())
            .await
            .expect("shutdown must stop the idle worker without waiting for the interval");
    }
}
