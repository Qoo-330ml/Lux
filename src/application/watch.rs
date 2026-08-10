use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use notify::{
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
    event::{ModifyKind, RenameMode},
};
use tokio::sync::mpsc;
use tokio::{
    sync::Mutex,
    task::{AbortHandle, JoinSet},
    time::sleep,
};

use crate::{
    application::scanner::{IncrementalScanChange, ScanJobService},
    domain::ids::LibraryId,
    storage::{Database, StoredLibraryRoot},
};

const EVENT_CHANNEL_CAPACITY: usize = 256;
const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ChangeKind {
    Create,
    Modify,
    Rename,
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileChange {
    pub path: PathBuf,
    pub kind: ChangeKind,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WatchStats {
    pub dropped_events: u64,
}

pub struct LibraryWatcher {
    root: PathBuf,
    watcher: RecommendedWatcher,
    receiver: mpsc::Receiver<FileChange>,
    dropped_events: Arc<AtomicU64>,
}

impl LibraryWatcher {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, WatchError> {
        let root = normalize_root(root.as_ref())?;
        let (sender, receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let dropped_events = Arc::new(AtomicU64::new(0));
        let dropped_for_callback = Arc::clone(&dropped_events);
        let root_for_callback = root.clone();
        let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
            let Ok(event) = result else {
                return;
            };
            for change in classify_event(&root_for_callback, event) {
                if sender.try_send(change).is_err() {
                    dropped_for_callback.fetch_add(1, Ordering::Relaxed);
                }
            }
        })
        .map_err(|error| WatchError::Notify(error.to_string()))?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|error| WatchError::Notify(error.to_string()))?;
        Ok(Self {
            root,
            watcher,
            receiver,
            dropped_events,
        })
    }

    pub async fn next_batch(&mut self) -> Option<Vec<FileChange>> {
        let first = self.receiver.recv().await?;
        let mut coalescer = EventCoalescer::new(DEFAULT_DEBOUNCE);
        coalescer.push(first);
        let deadline = tokio::time::sleep(coalescer.debounce());
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                event = self.receiver.recv() => match event {
                    Some(event) => coalescer.push(event),
                    None => break,
                },
                _ = &mut deadline => break,
            }
        }
        Some(coalescer.finish())
    }

    pub fn stats(&self) -> WatchStats {
        WatchStats {
            dropped_events: self.dropped_events.load(Ordering::Relaxed),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn channel_capacity() -> usize {
        EVENT_CHANNEL_CAPACITY
    }

    pub fn watcher_alive(&self) -> bool {
        let _ = &self.watcher;
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WatcherToken(u64);

#[derive(Debug)]
struct ActiveWatcher {
    token: WatcherToken,
    abort_handle: AbortHandle,
}

#[derive(Debug)]
struct WatcherTaskResult {
    root_id: String,
    token: WatcherToken,
}

#[derive(Clone)]
pub struct LibraryWatchService {
    database: Database,
    scan_jobs: ScanJobService,
}

impl LibraryWatchService {
    pub fn new(database: Database) -> Self {
        let scan_jobs = ScanJobService::new(database.clone());
        Self::with_scan_jobs(database, scan_jobs)
    }

    pub fn with_scan_jobs(database: Database, scan_jobs: ScanJobService) -> Self {
        Self {
            scan_jobs,
            database,
        }
    }

    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            self.run().await;
        })
    }

    async fn run(self) {
        let mut active_roots = HashMap::<String, ActiveWatcher>::new();
        let mut watcher_tasks = JoinSet::new();
        let running_jobs = Arc::new(Mutex::new(HashSet::<String>::new()));
        let mut next_token = 0;
        self.refresh_roots(
            &mut active_roots,
            &mut watcher_tasks,
            &running_jobs,
            &mut next_token,
        )
        .await;
        loop {
            tokio::select! {
                _ = sleep(Duration::from_secs(1)) => {
                    self.refresh_roots(
                        &mut active_roots,
                        &mut watcher_tasks,
                        &running_jobs,
                        &mut next_token,
                    ).await;
                }
                Some(result) = watcher_tasks.join_next() => {
                    if let Ok(result) = result {
                        remove_completed_watcher(&mut active_roots, result);
                    }
                }
            }
        }
    }

    async fn refresh_roots(
        &self,
        active_roots: &mut HashMap<String, ActiveWatcher>,
        watcher_tasks: &mut JoinSet<WatcherTaskResult>,
        running_jobs: &Arc<Mutex<HashSet<String>>>,
        next_token: &mut u64,
    ) {
        match self.database.list_enabled_library_roots().await {
            Ok(roots) => {
                let enabled_root_ids = roots
                    .iter()
                    .map(|root| root.id.clone())
                    .collect::<HashSet<_>>();
                reconcile_active_roots(active_roots, &enabled_root_ids);
                for root in roots {
                    if active_roots.contains_key(&root.id) {
                        continue;
                    }
                    let service = self.clone();
                    let running_jobs = Arc::clone(running_jobs);
                    let root_id = root.id.clone();
                    let token = next_watcher_token(next_token);
                    let watcher_handle = watcher_tasks.spawn(async move {
                        let root_id = root.id.clone();
                        service.watch_root(root, running_jobs).await;
                        WatcherTaskResult { root_id, token }
                    });
                    active_roots.insert(
                        root_id,
                        ActiveWatcher {
                            token,
                            abort_handle: watcher_handle,
                        },
                    );
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to refresh library watch roots");
            }
        }
    }

    async fn watch_root(&self, root: StoredLibraryRoot, running_jobs: Arc<Mutex<HashSet<String>>>) {
        let root_path = root.canonical_path.clone();
        let mut watcher = match tokio::task::spawn_blocking(move || LibraryWatcher::new(root_path))
            .await
        {
            Ok(Ok(watcher)) => watcher,
            Ok(Err(error)) => {
                tracing::warn!(root_id = %root.id, %error, "library root realtime watch unavailable");
                return;
            }
            Err(error) => {
                tracing::warn!(root_id = %root.id, %error, "library root realtime watch worker stopped");
                return;
            }
        };
        while let Some(batch) = watcher.next_batch().await {
            let changes = batch
                .into_iter()
                .filter_map(|change| {
                    let relative_path = change
                        .path
                        .strip_prefix(&root.canonical_path)
                        .ok()?
                        .to_str()?
                        .to_owned();
                    (!relative_path.is_empty()).then_some(IncrementalScanChange {
                        root_id: root.id.clone(),
                        relative_path,
                        kind: change.kind,
                    })
                })
                .collect::<Vec<_>>();
            if changes.is_empty() {
                continue;
            }
            let Ok(library_id) = root.library_id.parse::<LibraryId>() else {
                tracing::warn!(root_id = %root.id, "realtime watch skipped invalid library ID");
                continue;
            };
            match self
                .scan_jobs
                .enqueue_incremental_changes(library_id, changes)
                .await
            {
                Ok(job) => {
                    let mut running = running_jobs.lock().await;
                    if !running.insert(job.id.clone()) {
                        continue;
                    }
                    drop(running);
                    let scan_jobs = self.scan_jobs.clone();
                    let running_jobs = Arc::clone(&running_jobs);
                    let job_id = job.id.clone();
                    tokio::spawn(async move {
                        if let Err(error) = scan_jobs.run_to_completion(&job_id, 100, None).await {
                            tracing::error!(job_id = %job_id, %error, "realtime incremental scan stopped");
                        }
                        running_jobs.lock().await.remove(&job_id);
                    });
                }
                Err(error) => {
                    tracing::warn!(library_id = %library_id, %error, "realtime incremental scan was not queued");
                }
            }
        }
    }
}

fn reconcile_active_roots(
    active_roots: &mut HashMap<String, ActiveWatcher>,
    enabled_root_ids: &HashSet<String>,
) {
    let stale_root_ids = active_roots
        .keys()
        .filter(|root_id| !enabled_root_ids.contains(*root_id))
        .cloned()
        .collect::<Vec<_>>();
    for root_id in stale_root_ids {
        if let Some(active_watcher) = active_roots.remove(&root_id) {
            active_watcher.abort_handle.abort();
        }
    }
}

fn next_watcher_token(next_token: &mut u64) -> WatcherToken {
    let token = WatcherToken(*next_token);
    *next_token = next_token.wrapping_add(1);
    token
}

fn remove_completed_watcher(
    active_roots: &mut HashMap<String, ActiveWatcher>,
    completed: WatcherTaskResult,
) {
    if active_roots
        .get(&completed.root_id)
        .is_some_and(|active| active.token == completed.token)
    {
        active_roots.remove(&completed.root_id);
    }
}

pub struct EventCoalescer {
    debounce: Duration,
    changes: BTreeMap<PathBuf, ChangeKind>,
}

impl EventCoalescer {
    pub fn new(debounce: Duration) -> Self {
        Self {
            debounce,
            changes: BTreeMap::new(),
        }
    }

    pub fn push(&mut self, change: FileChange) {
        self.changes
            .entry(change.path)
            .and_modify(|kind| *kind = merge_kind(*kind, change.kind))
            .or_insert(change.kind);
    }

    pub fn finish(self) -> Vec<FileChange> {
        self.changes
            .into_iter()
            .map(|(path, kind)| FileChange { path, kind })
            .collect()
    }

    fn debounce(&self) -> Duration {
        self.debounce
    }
}

fn merge_kind(previous: ChangeKind, next: ChangeKind) -> ChangeKind {
    match (previous, next) {
        (ChangeKind::Remove, ChangeKind::Create) => ChangeKind::Modify,
        (ChangeKind::Create, ChangeKind::Modify) => ChangeKind::Create,
        (_, ChangeKind::Remove) => ChangeKind::Remove,
        (_, ChangeKind::Rename) => ChangeKind::Rename,
        (ChangeKind::Rename, _) => ChangeKind::Rename,
        (ChangeKind::Create, _) => ChangeKind::Create,
        _ => ChangeKind::Modify,
    }
}

fn classify_event(root: &Path, event: Event) -> Vec<FileChange> {
    let kind = match event.kind {
        EventKind::Create(_) => ChangeKind::Create,
        EventKind::Remove(_) => ChangeKind::Remove,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both))
        | EventKind::Modify(ModifyKind::Name(RenameMode::From))
        | EventKind::Modify(ModifyKind::Name(RenameMode::To))
        | EventKind::Modify(ModifyKind::Name(RenameMode::Any)) => ChangeKind::Rename,
        EventKind::Modify(_) => ChangeKind::Modify,
        _ => return Vec::new(),
    };
    event
        .paths
        .into_iter()
        .filter_map(|path| normalize_event_path(root, path).map(|path| FileChange { path, kind }))
        .collect()
}

fn normalize_root(root: &Path) -> Result<PathBuf, WatchError> {
    std::fs::canonicalize(root).map_err(|source| WatchError::Io {
        path: root.to_owned(),
        source,
    })
}

fn normalize_event_path(root: &Path, path: PathBuf) -> Option<PathBuf> {
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    if path.starts_with(root) {
        Some(path)
    } else {
        None
    }
}

#[derive(Debug)]
pub enum WatchError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Notify(String),
}

impl fmt::Display for WatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "watch root '{}': {source}", path.display())
            }
            Self::Notify(error) => write!(formatter, "file watcher failed: {error}"),
        }
    }
}

impl std::error::Error for WatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Notify(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        future::pending,
    };

    use super::*;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn disabled_root_aborts_its_watch_task() {
        let mut watcher_tasks = JoinSet::new();
        let watcher_handle = watcher_tasks.spawn(async { pending::<WatcherTaskResult>().await });
        let mut active_roots = HashMap::from([(
            String::from("root-1"),
            ActiveWatcher {
                token: WatcherToken(1),
                abort_handle: watcher_handle,
            },
        )]);

        reconcile_active_roots(&mut active_roots, &HashSet::from([String::from("root-2")]));

        assert!(active_roots.is_empty());
        let result = watcher_tasks
            .join_next()
            .await
            .expect("aborted watcher should be joinable")
            .expect_err("aborted watcher must not return normally");
        assert!(result.is_cancelled());
    }

    #[tokio::test]
    async fn old_watcher_result_does_not_remove_replacement() {
        let mut watcher_tasks = JoinSet::new();
        let (complete_old_tx, complete_old_rx) = oneshot::channel();
        let old_result = WatcherTaskResult {
            root_id: String::from("root-1"),
            token: WatcherToken(1),
        };
        let old_handle = watcher_tasks.spawn(async move {
            let _ = complete_old_rx.await;
            old_result
        });
        let mut active_roots = HashMap::from([(
            String::from("root-1"),
            ActiveWatcher {
                token: WatcherToken(1),
                abort_handle: old_handle.clone(),
            },
        )]);
        complete_old_tx
            .send(())
            .expect("old watcher should still be waiting");
        while !old_handle.is_finished() {
            tokio::task::yield_now().await;
        }

        reconcile_active_roots(&mut active_roots, &HashSet::new());

        let replacement_handle =
            watcher_tasks.spawn(async { pending::<WatcherTaskResult>().await });
        active_roots.insert(
            String::from("root-1"),
            ActiveWatcher {
                token: WatcherToken(2),
                abort_handle: replacement_handle,
            },
        );

        let completed = watcher_tasks
            .join_next()
            .await
            .expect("old watcher result should be available")
            .expect("old watcher should finish normally");
        remove_completed_watcher(&mut active_roots, completed);

        assert_eq!(
            active_roots.get("root-1").map(|active| active.token),
            Some(WatcherToken(2))
        );
        watcher_tasks.abort_all();
    }
}
