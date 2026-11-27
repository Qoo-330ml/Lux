use crate::{
    application::{
        admin_cache,
        admin_events::{AdminEventHub, AdminEventScope},
        catalog::CatalogService,
        home::HomeService,
        item_merge::{IDENTITY_MERGE_GROUP_BUDGET, IdentityMergeReport, ItemMergeService},
    },
    observability::logs::{LogStore, NewAuditLogEvent},
    storage::Database,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Notify, watch},
    task::JoinHandle,
};
use uuid::Uuid;

/// Coalescing window after a storage outbox write before sweeping for duplicate identities.
const IDENTITY_MERGE_COALESCE_DELAY: Duration = Duration::from_secs(2);
const IDENTITY_MERGE_RETRY_INTERVAL: Duration = Duration::from_secs(300);
/// Pause between consecutive passes while a large backfill is still producing merges.
const IDENTITY_MERGE_DRAIN_PAUSE: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub(crate) struct IdentityMergeWorker {
    shutdown_sender: watch::Sender<bool>,
    task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

impl IdentityMergeWorker {
    pub(crate) fn start(
        database: Database,
        home: Option<HomeService>,
        catalog: Option<CatalogService>,
        admin_events: AdminEventHub,
        config_dir: PathBuf,
        notify: Arc<Notify>,
    ) -> Self {
        Self::start_with_intervals(
            database,
            home,
            catalog,
            admin_events,
            config_dir,
            notify,
            IDENTITY_MERGE_COALESCE_DELAY,
            IDENTITY_MERGE_RETRY_INTERVAL,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn start_with_intervals(
        database: Database,
        home: Option<HomeService>,
        catalog: Option<CatalogService>,
        admin_events: AdminEventHub,
        config_dir: PathBuf,
        notify: Arc<Notify>,
        coalesce_delay: Duration,
        retry_interval: Duration,
    ) -> Self {
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let task = tokio::spawn(run_identity_merge_worker(
            database,
            home,
            catalog,
            admin_events,
            config_dir,
            notify,
            coalesce_delay,
            retry_interval,
            shutdown_receiver,
        ));
        Self {
            shutdown_sender,
            task: Arc::new(tokio::sync::Mutex::new(Some(task))),
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.shutdown_sender.send_replace(true);
        if let Some(task) = self.task.lock().await.take()
            && let Err(error) = task.await
        {
            tracing::warn!(%error, "identity merge worker stopped unexpectedly");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_identity_merge_worker(
    database: Database,
    home: Option<HomeService>,
    catalog: Option<CatalogService>,
    admin_events: AdminEventHub,
    config_dir: PathBuf,
    notify: Arc<Notify>,
    coalesce_delay: Duration,
    retry_interval: Duration,
    mut shutdown_receiver: watch::Receiver<bool>,
) {
    let signals = database.outbox_signals();
    let service = ItemMergeService::new(database);
    loop {
        if *shutdown_receiver.borrow() {
            return;
        }
        let mut drained = false;
        let mut had_error = false;
        match service
            .merge_identity_duplicates(None, IDENTITY_MERGE_GROUP_BUDGET)
            .await
        {
            Ok(report) => {
                if report.changed() {
                    drained = true;
                    tracing::info!(
                        groups_merged = report.groups_merged,
                        items_merged = report.items_merged,
                        groups_skipped = report.groups_skipped,
                        "merged duplicate media identities"
                    );
                    admin_cache::invalidate_media_catalog(home.as_ref(), catalog.as_ref());
                    admin_events.publish(AdminEventScope::Metadata);
                    record_auto_merge_audit_event(&config_dir, &report).await;
                }
            }
            Err(error) => {
                had_error = true;
                tracing::warn!(%error, "automatic media identity merge failed; will retry");
            }
        }
        if drained {
            // Keep converging until no duplicate group remains, but yield between passes so a large
            // backfill does not monopolize the database or the background budget.
            tokio::select! {
                _ = tokio::time::sleep(IDENTITY_MERGE_DRAIN_PAUSE) => {}
                _ = shutdown_receiver.changed() => return,
            }
            continue;
        }
        if *shutdown_receiver.borrow() {
            return;
        }
        if had_error {
            tokio::select! {
                _ = notify.notified() => {}
                () = signals.notified_identity_merge() => {}
                _ = tokio::time::sleep(retry_interval) => {}
                _ = shutdown_receiver.changed() => return,
            }
            continue;
        }
        let coalesce = tokio::select! {
            _ = notify.notified() => false,
            () = signals.notified_identity_merge() => true,
            _ = shutdown_receiver.changed() => return,
        };
        if coalesce && !coalesce_delay.is_zero() {
            tokio::select! {
                _ = notify.notified() => {}
                _ = tokio::time::sleep(coalesce_delay) => {}
                _ = shutdown_receiver.changed() => return,
            }
        }
    }
}

async fn record_auto_merge_audit_event(config_dir: &Path, report: &IdentityMergeReport) {
    let metadata_json = serde_json::json!({
        "origin": "AUTO",
        "groupsMerged": report.groups_merged,
        "itemsMerged": report.items_merged,
        "groupsSkipped": report.groups_skipped,
    })
    .to_string();
    let event_id = Uuid::now_v7().to_string();
    if let Err(error) = LogStore::new(config_dir)
        .append_audit_event(NewAuditLogEvent {
            id: &event_id,
            actor_user_id: None,
            actor_username: None,
            event_type: "MEDIA_ITEMS_AUTO_MERGED",
            target_type: Some("media_items"),
            target_id: None,
            metadata_json: &metadata_json,
        })
        .await
    {
        tracing::warn!(
            ?error,
            "failed to persist automatic media merge audit event"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};
    use tokio::sync::Notify;

    use super::IdentityMergeWorker;
    use crate::{application::admin_events::AdminEventHub, storage::test_database};

    #[tokio::test]
    async fn idle_worker_executes_zero_queries_after_initial_pass_and_wakes_on_signal()
    -> Result<(), Box<dyn std::error::Error>> {
        let (temp_dir, database) = test_database().await?;
        let notify = Arc::new(Notify::new());
        let worker = IdentityMergeWorker::start_with_intervals(
            database.clone(),
            None,
            None,
            AdminEventHub::default(),
            temp_dir.path().join("config"),
            notify.clone(),
            Duration::from_millis(25),
            Duration::from_millis(50),
        );

        tokio::time::sleep(Duration::from_millis(40)).await;
        database.reset_query_count();

        tokio::time::sleep(Duration::from_millis(90)).await;
        assert_eq!(
            database.query_count(),
            0,
            "idle identity merge worker must not poll the database on a timer"
        );

        notify.notify_one();
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(
            database.query_count() >= 2,
            "explicit notify must wake the identity merge worker immediately"
        );

        database.reset_query_count();
        database.outbox_signals().notify_writers();
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(
            database.query_count() >= 2,
            "catalog outbox signal must wake the identity merge worker after coalescing"
        );

        worker.shutdown().await;
        Ok(())
    }
}
