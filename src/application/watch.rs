use std::{
    collections::BTreeMap,
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
