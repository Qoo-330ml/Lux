use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::application::{candidates::MetadataSelectionService, scraper::ScraperProvider};

const ACTOR_ENRICHMENT_QUEUE_CAPACITY: usize = 256;
const ACTOR_ENRICHMENT_WORKERS: usize = 2;

struct ActorEnrichmentTask {
    key: String,
    item_id: String,
    candidate_id: String,
    selection: MetadataSelectionService,
    scraper: ScraperProvider,
    _queue_state: Arc<ActorEnrichmentQueueState>,
}

struct ActorEnrichmentQueueState {
    sender: mpsc::Sender<ActorEnrichmentTask>,
    queued: Arc<AsyncMutex<HashSet<String>>>,
    cancellation: CancellationToken,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for ActorEnrichmentQueueState {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Ok(mut workers) = self.workers.lock() {
            for worker in workers.drain(..) {
                worker.abort();
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct ActorEnrichmentQueue {
    state: Arc<ActorEnrichmentQueueState>,
}

impl ActorEnrichmentQueue {
    pub(crate) fn new() -> Self {
        let (sender, receiver) =
            mpsc::channel::<ActorEnrichmentTask>(ACTOR_ENRICHMENT_QUEUE_CAPACITY);
        let receiver = Arc::new(AsyncMutex::new(receiver));
        let queued = Arc::new(AsyncMutex::new(HashSet::new()));
        let cancellation = CancellationToken::new();
        let mut workers = Vec::with_capacity(ACTOR_ENRICHMENT_WORKERS);

        for _ in 0..ACTOR_ENRICHMENT_WORKERS {
            let receiver = Arc::clone(&receiver);
            let queued = Arc::clone(&queued);
            let cancellation = cancellation.clone();
            workers.push(tokio::spawn(async move {
                loop {
                    let task = tokio::select! {
                        _ = cancellation.cancelled() => break,
                        task = async { receiver.lock().await.recv().await } => task,
                    };
                    let Some(task) = task else {
                        break;
                    };
                    let key = task.key.clone();
                    if let Err(error) = task
                        .selection
                        .enrich_selected_actors(&task.item_id, &task.candidate_id, &task.scraper)
                        .await
                    {
                        tracing::warn!(
                            item_id = %task.item_id,
                            candidate_id = %task.candidate_id,
                            %error,
                            "actor metadata enrichment failed"
                        );
                    }
                    queued.lock().await.remove(&key);
                }
            }));
        }

        Self {
            state: Arc::new(ActorEnrichmentQueueState {
                sender,
                queued,
                cancellation,
                workers: Mutex::new(workers),
            }),
        }
    }

    pub(crate) async fn enqueue(
        &self,
        item_id: &str,
        candidate_id: &str,
        selection: MetadataSelectionService,
        scraper: ScraperProvider,
    ) -> bool {
        let key = format!("{item_id}:{candidate_id}");
        {
            let mut queued = self.state.queued.lock().await;
            if !queued.insert(key.clone()) {
                return true;
            }
        }

        let task = ActorEnrichmentTask {
            key: key.clone(),
            item_id: item_id.to_owned(),
            candidate_id: candidate_id.to_owned(),
            selection,
            scraper,
            _queue_state: Arc::clone(&self.state),
        };
        if self.state.sender.try_send(task).is_err() {
            self.state.queued.lock().await.remove(&key);
            return false;
        }
        true
    }
}
