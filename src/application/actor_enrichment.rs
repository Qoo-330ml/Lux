use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::{future::Future, pin::Pin};

use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::application::{candidates::MetadataSelectionService, scraper::ScraperProvider};

const ACTOR_ENRICHMENT_QUEUE_CAPACITY: usize = 256;
const ACTOR_ENRICHMENT_WORKERS: usize = 2;

struct ActorEnrichmentTask {
    _queued_key: QueuedActorEnrichmentKey,
    _queue_state: Arc<ActorEnrichmentQueueState>,
    work: Pin<Box<dyn Future<Output = ()> + Send>>,
}

struct QueuedActorEnrichmentKey {
    key: String,
    queued: Arc<Mutex<HashSet<String>>>,
}

impl Drop for QueuedActorEnrichmentKey {
    fn drop(&mut self) {
        if let Ok(mut queued) = self.queued.lock() {
            queued.remove(&self.key);
        }
    }
}

struct ActorEnrichmentQueueState {
    sender: Mutex<Option<mpsc::Sender<ActorEnrichmentTask>>>,
    queued: Arc<Mutex<HashSet<String>>>,
    cancellation: CancellationToken,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for ActorEnrichmentQueueState {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.sender.get_mut().ok().and_then(Option::take);
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
        Self::with_limits(ACTOR_ENRICHMENT_QUEUE_CAPACITY, ACTOR_ENRICHMENT_WORKERS)
    }

    fn with_limits(capacity: usize, worker_count: usize) -> Self {
        let (sender, receiver) = mpsc::channel::<ActorEnrichmentTask>(capacity);
        let receiver = Arc::new(AsyncMutex::new(receiver));
        let queued = Arc::new(Mutex::new(HashSet::new()));
        let cancellation = CancellationToken::new();
        let mut workers = Vec::with_capacity(worker_count);

        for _ in 0..worker_count {
            let receiver = Arc::clone(&receiver);
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
                    let ActorEnrichmentTask {
                        _queued_key,
                        _queue_state,
                        work,
                    } = task;
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => break,
                        _ = work => {},
                    }
                }
            }));
        }

        Self {
            state: Arc::new(ActorEnrichmentQueueState {
                sender: Mutex::new(Some(sender)),
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
        let item_id = item_id.to_owned();
        let candidate_id = candidate_id.to_owned();
        self.enqueue_work(key, async move {
            if let Err(error) = selection
                .enrich_selected_actors(&item_id, &candidate_id, &scraper)
                .await
            {
                tracing::warn!(%item_id, %candidate_id, %error, "actor metadata enrichment failed");
            }
        })
        .await
    }

    async fn enqueue_work(
        &self,
        key: String,
        work: impl Future<Output = ()> + Send + 'static,
    ) -> bool {
        if self.state.cancellation.is_cancelled() {
            return false;
        }
        {
            let Ok(queued) = self.state.queued.lock() else {
                return false;
            };
            if queued.contains(&key) {
                return true;
            }
        }
        let sender = self
            .state
            .sender
            .lock()
            .ok()
            .and_then(|sender| sender.as_ref().cloned());
        let Some(sender) = sender else {
            return false;
        };
        tokio::select! {
            biased;
            _ = self.state.cancellation.cancelled() => false,
            result = sender.reserve() => match result {
                Ok(permit) => {
                    let Ok(mut queued) = self.state.queued.lock() else {
                        return false;
                    };
                    if self.state.cancellation.is_cancelled() {
                        return false;
                    }
                    if !queued.insert(key.clone()) {
                        return true;
                    }
                    drop(queued);
                    permit.send(ActorEnrichmentTask {
                        _queued_key: QueuedActorEnrichmentKey { key, queued: Arc::clone(&self.state.queued) },
                        _queue_state: Arc::clone(&self.state),
                        work: Box::pin(work),
                    });
                    true
                }
                Err(_) => false,
            },
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.state.cancellation.cancel();
        self.state
            .sender
            .lock()
            .ok()
            .and_then(|mut sender| sender.take());
        let workers = self
            .state
            .workers
            .lock()
            .map(|mut workers| workers.drain(..).collect::<Vec<_>>())
            .unwrap_or_default();
        for worker in workers {
            worker.abort();
            let _ = worker.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ActorEnrichmentQueue, QueuedActorEnrichmentKey};
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::{Notify, oneshot};

    #[tokio::test]
    async fn full_queue_waits_for_capacity_and_runs_every_accepted_task() {
        let queue = ActorEnrichmentQueue::with_limits(1, 1);
        let release = Arc::new(Notify::new());
        let (started_tx, started_rx) = oneshot::channel();
        let worker_release = release.clone();
        assert!(
            queue
                .enqueue_work("active".into(), async move {
                    let _ = started_tx.send(());
                    worker_release.notified().await;
                })
                .await
        );
        started_rx.await.expect("active worker started");
        let (second_tx, second_rx) = oneshot::channel();
        assert!(
            queue
                .enqueue_work("queued".into(), async move {
                    let _ = second_tx.send(());
                })
                .await
        );
        let (third_tx, third_rx) = oneshot::channel();
        let third = queue.enqueue_work("waiting".into(), async move {
            let _ = third_tx.send(());
        });
        tokio::pin!(third);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut third)
                .await
                .is_err()
        );
        release.notify_one();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), third)
                .await
                .expect("capacity released")
        );
        second_rx.await.expect("second task completed");
        third_rx.await.expect("third task completed");
        queue.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_cancels_active_queued_and_waiting_work() {
        let queue = ActorEnrichmentQueue::with_limits(1, 1);
        let (started_tx, started_rx) = oneshot::channel();
        let (active_tx, active_rx) = oneshot::channel::<()>();
        assert!(
            queue
                .enqueue_work("active".into(), async move {
                    let _active_tx = active_tx;
                    let _ = started_tx.send(());
                    std::future::pending::<()>().await;
                })
                .await
        );
        started_rx.await.expect("worker started");
        let (queued_tx, queued_rx) = oneshot::channel();
        assert!(
            queue
                .enqueue_work("queued".into(), async move {
                    let _ = queued_tx.send(());
                })
                .await
        );
        let waiting = queue.enqueue_work("waiting".into(), std::future::pending());
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                .await
                .is_err()
        );
        queue.shutdown().await;
        assert!(!waiting.await);
        assert!(active_rx.await.is_err());
        assert!(queued_rx.await.is_err());
        assert!(queue.state.queued.lock().expect("queued state").is_empty());
        assert!(!queue.enqueue_work("after shutdown".into(), async {}).await);
    }

    #[tokio::test]
    async fn dropping_queue_owner_keeps_accepted_work_alive() {
        let queue = ActorEnrichmentQueue::with_limits(1, 1);
        let release = Arc::new(Notify::new());
        let (started_tx, started_rx) = oneshot::channel();
        let (completed_tx, completed_rx) = oneshot::channel::<()>();
        let worker_release = release.clone();
        assert!(
            queue
                .enqueue_work("active".into(), async move {
                    let _ = started_tx.send(());
                    worker_release.notified().await;
                    let _ = completed_tx.send(());
                })
                .await
        );
        started_rx.await.expect("worker started");
        let weak = Arc::downgrade(&queue.state);
        drop(queue);
        release.notify_one();
        completed_rx.await.expect("accepted work completed");
        for _ in 0..20 {
            if weak.upgrade().is_none() {
                return;
            }
            tokio::task::yield_now().await;
        }
        assert!(weak.upgrade().is_none());
    }

    #[tokio::test]
    async fn cancelled_enqueue_does_not_block_a_later_request_for_the_same_key() {
        let queue = ActorEnrichmentQueue::with_limits(1, 1);
        let release = Arc::new(Notify::new());
        let (started_tx, started_rx) = oneshot::channel();
        let worker_release = release.clone();
        assert!(
            queue
                .enqueue_work("active".into(), async move {
                    let _ = started_tx.send(());
                    worker_release.notified().await;
                })
                .await
        );
        started_rx.await.expect("worker started");
        assert!(queue.enqueue_work("queued".into(), async {}).await);
        {
            let waiting = queue.enqueue_work("retry".into(), async {});
            tokio::pin!(waiting);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut waiting)
                    .await
                    .is_err()
            );
        }
        let (done_tx, done_rx) = oneshot::channel();
        release.notify_one();
        assert!(
            queue
                .enqueue_work("retry".into(), async move {
                    let _ = done_tx.send(());
                })
                .await
        );
        tokio::time::timeout(Duration::from_secs(2), done_rx)
            .await
            .expect("retry ran")
            .expect("completed");
        queue.shutdown().await;
    }

    #[test]
    fn dropping_a_queued_key_releases_deduplication_state() {
        let queued = Arc::new(Mutex::new(HashSet::from(["item:candidate".to_owned()])));
        {
            let _key = QueuedActorEnrichmentKey {
                key: "item:candidate".to_owned(),
                queued: Arc::clone(&queued),
            };
        }
        assert!(queued.lock().expect("queued state lock").is_empty());
    }

    #[tokio::test]
    async fn shutdown_cancels_workers_and_closes_the_sender() {
        let queue = ActorEnrichmentQueue::new();
        queue.shutdown().await;
        assert!(queue.state.cancellation.is_cancelled());
        assert!(queue.state.sender.lock().expect("sender lock").is_none());
        assert!(queue.state.workers.lock().expect("worker lock").is_empty());
    }
}
