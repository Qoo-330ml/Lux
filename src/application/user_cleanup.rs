use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{application::user_avatars::UserAvatarService, domain::ids::UserId, storage::Database};

const USER_CLEANUP_INTERVAL: Duration = Duration::from_secs(60);
const USER_CLEANUP_BATCH_SIZE: i64 = 50;

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
        }
    });
}
