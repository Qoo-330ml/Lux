use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{
    application::user_avatars::UserAvatarService,
    domain::ids::UserId,
    storage::{Database, StorageError},
};

const USER_CLEANUP_INTERVAL: Duration = Duration::from_secs(60);
const USER_CLEANUP_BATCH_SIZE: i64 = 50;
const USER_ACTIVITY_RETENTION_BATCH_SIZE: i64 = 1_000;
const SECONDS_PER_DAY: i64 = 86_400;

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

pub(crate) fn spawn_user_cleanup_worker(database: Database, avatars: UserAvatarService) {
    tokio::spawn(async move {
        let first_tick = tokio::time::Instant::now() + USER_CLEANUP_INTERVAL;
        let mut interval = tokio::time::interval_at(first_tick, USER_CLEANUP_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .min(i64::MAX as u64) as i64;
            let deleted_user_ids = match database
                .purge_expired_deleted_users(now, USER_CLEANUP_BATCH_SIZE)
                .await
            {
                Ok(user_ids) => user_ids,
                Err(error) => {
                    tracing::warn!(%error, "failed to purge expired user accounts");
                    continue;
                }
            };

            for user_id in deleted_user_ids {
                let Ok(user_id) = user_id.parse::<UserId>() else {
                    tracing::warn!("purged user has an invalid ID; avatar cleanup was skipped");
                    continue;
                };
                if let Err(error) = avatars.remove(user_id).await {
                    tracing::warn!(%error, "failed to remove avatar for a purged user");
                }
            }

            if let Err(error) = purge_expired_user_activity_batch(&database, now).await {
                tracing::warn!(%error, "failed to purge expired user activity events");
            }
        }
    });
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
}
