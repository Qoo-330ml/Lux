//! Supervised background polling loops.
//!
//! Application services used to `tokio::spawn` an endless loop and drop the handle. Nothing could
//! stop the loop on shutdown, and a panic inside it killed the only instance while the process
//! kept running, so background work silently stopped until the next restart. [`BackgroundLoop`]
//! keeps a stop handle and restarts the loop body after a panic, logging the failure.

use std::{future::Future, sync::Arc, time::Duration};

use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};

/// Delay before restarting a loop body that panicked, so a persistent panic cannot spin.
const PANIC_RESTART_DELAY: Duration = Duration::from_secs(1);

pub(crate) struct BackgroundLoop {
    name: &'static str,
    shutdown_sender: watch::Sender<bool>,
    task: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl BackgroundLoop {
    /// Spawns `run` under supervision. `run` receives the shutdown receiver and returns when the
    /// loop body ends, either because shutdown was requested or because the loop has nothing left
    /// to do.
    pub(crate) fn spawn<F, Fut>(name: &'static str, run: F) -> Self
    where
        F: Fn(watch::Receiver<bool>) -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let task = tokio::spawn(supervise(name, shutdown_receiver, run));
        Self {
            name,
            shutdown_sender,
            task: Arc::new(Mutex::new(Some(task))),
        }
    }

    /// Requests shutdown and waits for the loop body to finish.
    pub(crate) async fn shutdown(&self) {
        self.shutdown_sender.send_replace(true);
        if let Some(task) = self.task.lock().await.take()
            && let Err(error) = task.await
        {
            tracing::warn!(
                loop_name = self.name,
                %error,
                "background loop stopped unexpectedly"
            );
        }
    }
}

async fn supervise<F, Fut>(name: &'static str, mut shutdown: watch::Receiver<bool>, run: F)
where
    F: Fn(watch::Receiver<bool>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    loop {
        if *shutdown.borrow() {
            return;
        }
        match tokio::spawn(run(shutdown.clone())).await {
            Ok(()) => return,
            Err(error) => {
                tracing::error!(
                    loop_name = name,
                    %error,
                    "background loop panicked; restarting it"
                );
                if *shutdown.borrow() {
                    return;
                }
                tokio::select! {
                    () = tokio::time::sleep(PANIC_RESTART_DELAY) => {}
                    result = shutdown.changed() => {
                        if result.is_err() || *shutdown.borrow() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// Sleeps for `duration`, returning `true` when shutdown was requested first.
pub(crate) async fn wait_or_shutdown(
    shutdown: &mut watch::Receiver<bool>,
    duration: Duration,
) -> bool {
    if *shutdown.borrow() {
        return true;
    }
    tokio::select! {
        () = tokio::time::sleep(duration) => *shutdown.borrow(),
        result = shutdown.changed() => result.is_err() || *shutdown.borrow(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use tokio::sync::Mutex;

    use super::{BackgroundLoop, wait_or_shutdown};

    #[tokio::test]
    async fn restarting_a_panicking_loop_keeps_it_running() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let finished = Arc::new(Mutex::new(None));
        let loop_attempts = attempts.clone();
        let loop_finished = finished.clone();
        let worker = BackgroundLoop::spawn("panicking", move |mut shutdown| {
            let attempts = loop_attempts.clone();
            let finished = loop_finished.clone();
            async move {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("first iteration fails");
                }
                *finished.lock().await = Some(attempts.load(Ordering::SeqCst));
                while !wait_or_shutdown(&mut shutdown, Duration::from_secs(60)).await {}
            }
        });

        let mut observed = None;
        for _ in 0..250 {
            if let Some(attempt) = *finished.lock().await {
                observed = Some(attempt);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            observed,
            Some(2),
            "the loop body must run again after it panicked"
        );
        tokio::time::timeout(Duration::from_secs(5), worker.shutdown())
            .await
            .expect("shutdown must stop the restarted loop");
    }

    #[tokio::test]
    async fn shutdown_stops_a_running_loop_promptly() {
        let started = Arc::new(AtomicUsize::new(0));
        let loop_started = started.clone();
        let worker = BackgroundLoop::spawn("idle", move |mut shutdown| {
            let started = loop_started.clone();
            async move {
                started.fetch_add(1, Ordering::SeqCst);
                while !wait_or_shutdown(&mut shutdown, Duration::from_secs(60)).await {}
            }
        });
        for _ in 0..250 {
            if started.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(started.load(Ordering::SeqCst), 1);
        tokio::time::timeout(Duration::from_secs(5), worker.shutdown())
            .await
            .expect("shutdown must not wait for the next poll interval");
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "shutdown must not restart the loop"
        );
    }
}
