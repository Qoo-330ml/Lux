use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::{sync::Mutex, time::interval};

use crate::{
    application::{
        plugins::PluginService,
        schedule::{STRM_MEDIA_INFO_TASK_TYPE, parse_interval},
        strm_probe::{StrmProbeError, StrmProbeService},
    },
    storage::Database,
};

const POLL_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct ScheduledTaskService {
    database: Database,
    strm_probe: StrmProbeService,
    cursors: Arc<Mutex<HashMap<String, TaskCursor>>>,
}

#[derive(Clone)]
struct TaskCursor {
    schedule: String,
    next_run_at: Instant,
}

impl ScheduledTaskService {
    pub fn new(database: Database, _plugins: PluginService, strm_probe: StrmProbeService) -> Self {
        Self {
            database,
            strm_probe,
            cursors: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn spawn(&self) {
        let worker = self.clone();
        tokio::spawn(async move {
            let mut ticker = interval(POLL_INTERVAL);
            loop {
                ticker.tick().await;
                worker.run_once().await;
            }
        });
    }

    pub async fn run_once(&self) {
        let task = match self
            .database
            .find_scheduled_task_config("GLOBAL", "global", STRM_MEDIA_INFO_TASK_TYPE)
            .await
        {
            Ok(task) => task,
            Err(error) => {
                tracing::error!(%error, "failed to read STRM scheduled task");
                return;
            }
        };
        let Some(task) = task.filter(|task| {
            task.is_enabled
                && task.source_type == "PLUGIN"
                && task.plugin_id.as_deref() == Some("org.lux.strm-media-info")
        }) else {
            self.cursors.lock().await.remove(STRM_MEDIA_INFO_TASK_TYPE);
            return;
        };
        let Some(schedule) = task.cron_or_interval.as_deref() else {
            self.cursors.lock().await.remove(STRM_MEDIA_INFO_TASK_TYPE);
            return;
        };
        let interval = match parse_interval(schedule) {
            Ok(interval) => interval,
            Err(error) => {
                tracing::warn!(?error, "ignoring invalid STRM scheduled task interval");
                self.cursors.lock().await.remove(STRM_MEDIA_INFO_TASK_TYPE);
                return;
            }
        };
        let now = Instant::now();
        let due = {
            let mut cursors = self.cursors.lock().await;
            let cursor = cursors.get(STRM_MEDIA_INFO_TASK_TYPE);
            let due = cursor.is_none_or(|cursor| {
                cursor.schedule != schedule && is_due(now, None)
                    || cursor.schedule == schedule && is_due(now, Some(cursor.next_run_at))
            });
            if due {
                cursors.insert(
                    STRM_MEDIA_INFO_TASK_TYPE.to_owned(),
                    TaskCursor {
                        schedule: schedule.to_owned(),
                        next_run_at: now + interval,
                    },
                );
            }
            due
        };
        if !due {
            return;
        }
        let jobs = match self.strm_probe.create_configured_jobs().await {
            Ok(jobs) => jobs,
            Err(StrmProbeError::AlreadyActive) => return,
            Err(error) => {
                tracing::warn!(%error, "scheduled STRM probe did not start");
                return;
            }
        };
        for job in jobs {
            let worker = self.strm_probe.clone();
            tokio::spawn(async move {
                if let Err(error) = worker.run(&job.id).await {
                    tracing::error!(job_id = %job.id, %error, "scheduled STRM probe stopped");
                }
            });
        }
    }
}

fn is_due(now: Instant, next_run_at: Option<Instant>) -> bool {
    next_run_at.is_none_or(|next_run_at| next_run_at <= now)
}

#[cfg(test)]
mod tests {
    use super::is_due;
    use std::time::{Duration, Instant};

    #[test]
    fn schedule_is_due_without_cursor_or_after_deadline() {
        let now = Instant::now();
        assert!(is_due(now, None));
        assert!(is_due(now, Some(now - Duration::from_secs(1))));
        assert!(!is_due(now, Some(now + Duration::from_secs(1))));
    }
}
