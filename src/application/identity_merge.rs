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

/// Automatic convergence is a background sweep: it never runs on a user request path.
const IDENTITY_MERGE_INTERVAL: Duration = Duration::from_secs(120);
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
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let task = tokio::spawn(run_identity_merge_worker(
            database,
            home,
            catalog,
            admin_events,
            config_dir,
            notify,
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
    mut shutdown_receiver: watch::Receiver<bool>,
) {
    let service = ItemMergeService::new(database);
    loop {
        if *shutdown_receiver.borrow() {
            return;
        }
        let mut delay = IDENTITY_MERGE_INTERVAL;
        let mut drained = false;
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
                delay = IDENTITY_MERGE_RETRY_INTERVAL;
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
        tokio::select! {
            _ = notify.notified() => {}
            _ = tokio::time::sleep(delay) => {}
            _ = shutdown_receiver.changed() => return,
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
