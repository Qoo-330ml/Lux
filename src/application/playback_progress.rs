//! Coalesces repeated playback progress ticks into one database write.
//!
//! Emby clients and the Lux web player report progress every ten seconds, and only a few of those
//! reports can change anything beyond the stored position: a state change, a played flip, a
//! position that crosses the resume threshold, or the end of the session. Every other tick is a
//! plain progress tick, so the newest one represents all of them and the position the SQL keeps
//! (`MAX(stored, excluded)`) is the maximum either way.
//!
//! This module keeps those plain ticks in memory and writes at most one of them per
//! [`PLAYBACK_PROGRESS_FLUSH_WINDOW`]. The merge layer is bounded, it survives a failed write by
//! keeping the tick for the next pass, and it drains on shutdown. The accepted cost is a crash
//! losing at most one window of progress, and another device seeing the resume position up to one
//! window late.
//!
//! Readers must go through [`PlaybackProgressService::find_session`] and
//! [`PlaybackProgressService::list_sessions`]: the handlers compare the stored state, reuse the
//! stored duration, and detect the resume-threshold crossing from that value, so a pending tick
//! has to be visible to them exactly like a written one.

use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::{
    sync::{Mutex, Notify, watch},
    time::Instant,
};

use crate::{
    application::background_loop::{BackgroundLoop, wait_or_shutdown},
    storage::{
        Database, NewPlaybackEvent, PlaybackEventEffects, StorageError, StoredPlaybackSession,
        playback_reached_played_threshold,
    },
};

/// Maximum time between a plain progress tick and the write that carries it.
///
/// This is the window the project owner approved: a crash loses at most this much progress, and
/// other clients see the resume position at most this much late.
pub(crate) const PLAYBACK_PROGRESS_FLUSH_WINDOW: Duration = Duration::from_secs(30);

/// Upper bound of sessions whose progress may wait in memory.
///
/// Reaching it writes the oldest entry immediately, so the layer can never grow past it.
const MAX_PENDING_PROGRESS_SESSIONS: usize = 4_096;

/// Delay before a flush that failed is tried again, so a broken write cannot spin the loop.
const FAILED_FLUSH_RETRY_DELAY: Duration = Duration::from_secs(2);

const STOPPED_STATE: &str = "STOPPED";
const UNKNOWN_DEVICE: &str = "unknown";

/// One playback session, identified the same way `playback_sessions` identifies it.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ProgressKey {
    user_id: String,
    play_session_id: String,
}

/// A [`NewPlaybackEvent`] with owned strings, so it can wait in the merge layer.
#[derive(Clone, Debug)]
struct OwnedPlaybackEvent {
    user_id: String,
    item_id: String,
    media_source_id: Option<String>,
    play_session_id: String,
    device_id: String,
    client: Option<String>,
    device_name: Option<String>,
    client_version: Option<String>,
    device_type: Option<String>,
    remote_ip: Option<String>,
    state: String,
    position_ticks: i64,
    duration_ticks: Option<i64>,
    played_percent: i64,
    is_paused: bool,
    renew_admission: bool,
}

impl OwnedPlaybackEvent {
    fn from_event(event: &NewPlaybackEvent<'_>) -> Self {
        Self {
            user_id: event.user_id.to_owned(),
            item_id: event.item_id.to_owned(),
            media_source_id: event.media_source_id.map(str::to_owned),
            play_session_id: event.play_session_id.to_owned(),
            device_id: event.device_id.to_owned(),
            client: event.client.map(str::to_owned),
            device_name: event.device_name.map(str::to_owned),
            client_version: event.client_version.map(str::to_owned),
            device_type: event.device_type.map(str::to_owned),
            remote_ip: event.remote_ip.map(str::to_owned),
            state: event.state.to_owned(),
            position_ticks: event.position_ticks,
            duration_ticks: event.duration_ticks,
            played_percent: event.played_percent,
            is_paused: event.is_paused,
            renew_admission: event.renew_admission,
        }
    }

    fn key(&self) -> ProgressKey {
        ProgressKey {
            user_id: self.user_id.clone(),
            play_session_id: self.play_session_id.clone(),
        }
    }

    /// Folds an older event of the same session into this one.
    ///
    /// The session upsert keeps the maximum position and falls back to the stored duration when
    /// the newer event carries none, so the merged event has to do the same. Every other column is
    /// last-write-wins there and therefore comes from `self`.
    fn merge_older(&mut self, older: &OwnedPlaybackEvent) {
        self.position_ticks = self.position_ticks.max(older.position_ticks);
        if self.duration_ticks.is_none() {
            self.duration_ticks = older.duration_ticks;
        }
    }

    /// True when this tick may flip `user_item_state.is_played`.
    fn may_flip_played(&self) -> bool {
        self.duration_ticks.is_some_and(|duration_ticks| {
            playback_reached_played_threshold(
                self.position_ticks,
                duration_ticks,
                self.played_percent,
            )
        })
    }

    fn as_event<'a>(
        &'a self,
        previous_state: Option<&'a str>,
        previous_position_ticks: Option<i64>,
    ) -> NewPlaybackEvent<'a> {
        NewPlaybackEvent {
            user_id: &self.user_id,
            item_id: &self.item_id,
            media_source_id: self.media_source_id.as_deref(),
            play_session_id: &self.play_session_id,
            device_id: &self.device_id,
            client: self.client.as_deref(),
            device_name: self.device_name.as_deref(),
            client_version: self.client_version.as_deref(),
            device_type: self.device_type.as_deref(),
            remote_ip: self.remote_ip.as_deref(),
            state: &self.state,
            position_ticks: self.position_ticks,
            duration_ticks: self.duration_ticks,
            played_percent: self.played_percent,
            is_paused: self.is_paused,
            previous_state,
            previous_position_ticks,
            renew_admission: self.renew_admission,
        }
    }
}

/// A tick waiting for its write.
#[derive(Clone, Debug)]
struct PendingProgress {
    event: OwnedPlaybackEvent,
    /// Unix second the newest merged tick was reported, so readers see the same `last_event_at`
    /// the uncoalesced write would have stored.
    observed_at: i64,
    /// When this entry must be written, so no session waits longer than the flush window.
    due_at: Instant,
}

impl PendingProgress {
    fn new(event: OwnedPlaybackEvent, observed_at: i64, window: Duration) -> Self {
        Self {
            event,
            observed_at,
            due_at: Instant::now() + window,
        }
    }
}

struct ProgressState {
    pending: HashMap<ProgressKey, PendingProgress>,
}

struct Shared {
    database: Database,
    state: Mutex<ProgressState>,
    notify: Notify,
    window: Duration,
    max_pending: usize,
    flusher: Mutex<Option<BackgroundLoop>>,
}

/// The merge layer in front of [`Database::record_playback_event`].
#[derive(Clone)]
pub(crate) struct PlaybackProgressService {
    shared: Arc<Shared>,
}

impl PlaybackProgressService {
    pub(crate) fn new(database: Database) -> Self {
        Self::with_limits(
            database,
            PLAYBACK_PROGRESS_FLUSH_WINDOW,
            MAX_PENDING_PROGRESS_SESSIONS,
        )
    }

    /// Builds a service with an explicit window and entry bound.
    ///
    /// Only tests pass a value other than the production defaults; it is the injectable clock of
    /// this layer, because the window is the only thing that decides when a merged tick is due.
    pub(crate) fn with_limits(database: Database, window: Duration, max_pending: usize) -> Self {
        Self {
            shared: Arc::new(Shared {
                database,
                state: Mutex::new(ProgressState {
                    pending: HashMap::new(),
                }),
                notify: Notify::new(),
                window,
                max_pending: max_pending.max(1),
                flusher: Mutex::new(None),
            }),
        }
    }

    /// Starts the periodic flush loop once and records its stop handle.
    ///
    /// The loop sleeps until the earliest pending tick is due, and waits for a notification
    /// instead of polling while nothing is pending, so an idle server does not wake up for it.
    pub(crate) async fn spawn(&self) {
        let mut flusher = self.shared.flusher.lock().await;
        if flusher.is_some() {
            return;
        }
        let shared = self.shared.clone();
        *flusher = Some(BackgroundLoop::spawn(
            "playback_progress_flush",
            move |shutdown| {
                let shared = shared.clone();
                async move { run_flush_worker(shared, shutdown).await }
            },
        ));
    }

    /// Stops the flush loop and writes everything it still holds.
    ///
    /// This runs inside the shutdown drain: after it returns, no progress tick is left in memory.
    pub(crate) async fn shutdown(&self) {
        let flusher = self.shared.flusher.lock().await.take();
        if let Some(flusher) = flusher {
            flusher.shutdown().await;
        }
        // The loop drains on its way out; this covers a service that never started the loop and
        // any entry a failed write put back after that drain.
        self.shared.flush_all().await;
    }

    /// Records one playback event, writing it now or merging it into the pending progress.
    pub(crate) async fn record(
        &self,
        event: NewPlaybackEvent<'_>,
    ) -> Result<PlaybackEventEffects, StorageError> {
        self.shared.record(event).await
    }

    /// Reads a play session, including progress that is still pending in memory.
    pub(crate) async fn find_session(
        &self,
        user_id: &str,
        play_session_id: &str,
    ) -> Result<Option<StoredPlaybackSession>, StorageError> {
        let persisted = self
            .shared
            .database
            .find_playback_session(user_id, play_session_id)
            .await?;
        let pending = self.shared.pending_entry(user_id, play_session_id).await;
        Ok(overlay_session(persisted, pending.as_ref()))
    }

    /// Lists active play sessions, including progress that is still pending in memory.
    ///
    /// The database decides which sessions match the user and activity filters; a pending tick only
    /// replaces the values of a session the list already contains, because it never changes the
    /// state the filter reads.
    pub(crate) async fn list_sessions(
        &self,
        user_id: Option<&str>,
        active_within_seconds: Option<i64>,
    ) -> Result<Vec<StoredPlaybackSession>, StorageError> {
        let sessions = self
            .shared
            .database
            .list_playback_sessions(user_id, active_within_seconds)
            .await?;
        let pending = {
            let state = self.shared.state.lock().await;
            state.pending.clone()
        };
        Ok(sessions
            .into_iter()
            .map(|session| {
                let key = ProgressKey {
                    user_id: session.user_id.clone(),
                    play_session_id: session.play_session_id.clone(),
                };
                match pending.get(&key) {
                    Some(entry) => apply_pending(session, entry),
                    None => session,
                }
            })
            .collect())
    }

    /// Writes every pending tick now.
    #[cfg(test)]
    pub(crate) async fn flush_all(&self) {
        self.shared.flush_all().await;
    }

    /// Runs one periodic flush pass, like the background loop does.
    #[cfg(test)]
    pub(crate) async fn flush_due(&self) {
        self.shared.flush_due().await;
    }

    #[cfg(test)]
    pub(crate) async fn pending_len(&self) -> usize {
        self.shared.state.lock().await.pending.len()
    }
}

impl Shared {
    async fn record(
        &self,
        event: NewPlaybackEvent<'_>,
    ) -> Result<PlaybackEventEffects, StorageError> {
        let previous_state = event.previous_state.map(str::to_owned);
        let previous_position_ticks = event.previous_position_ticks;
        let owned = OwnedPlaybackEvent::from_event(&event);
        let key = owned.key();
        if self.pending_is_due(&key).await
            || self
                .should_write_through(&owned, previous_state.as_deref(), previous_position_ticks)
                .await?
        {
            return self
                .write_through(key, previous_state, previous_position_ticks, owned)
                .await;
        }
        self.defer(key, owned).await;
        Ok(PlaybackEventEffects::unchanged())
    }

    /// True when the event must reach the database before it changes anything a reader can see.
    ///
    /// These are exactly the triggers the uncoalesced path already applied: a state change (start,
    /// pause, resume, stop), a tick that may flip the played flag, and a tick that moves the
    /// position across the resume threshold, which is what adds an item to, or removes it from,
    /// continue-watching. A first event for a session has no previous position and is written too.
    async fn should_write_through(
        &self,
        event: &OwnedPlaybackEvent,
        previous_state: Option<&str>,
        previous_position_ticks: Option<i64>,
    ) -> Result<bool, StorageError> {
        if previous_state != Some(event.state.as_str()) || event.state == STOPPED_STATE {
            return Ok(true);
        }
        if event.may_flip_played() {
            return Ok(true);
        }
        let Some(previous_position_ticks) = previous_position_ticks else {
            return Ok(true);
        };
        let Some(threshold) = self
            .database
            .resume_threshold_for_item(&event.item_id)
            .await?
        else {
            return Ok(false);
        };
        Ok((previous_position_ticks < threshold) != (event.position_ticks < threshold))
    }

    /// Writes one event now, folding in whatever the same session still had pending.
    async fn write_through(
        &self,
        key: ProgressKey,
        previous_state: Option<String>,
        previous_position_ticks: Option<i64>,
        event: OwnedPlaybackEvent,
    ) -> Result<PlaybackEventEffects, StorageError> {
        let event = match self.take_pending(&key).await {
            Some(pending) => merge_into(event, &pending.event),
            None => event,
        };
        let result = self
            .database
            .record_playback_event(
                event.as_event(previous_state.as_deref(), previous_position_ticks),
            )
            .await;
        if result.is_err() {
            // The caller reports the failure, but the tick must not be lost: the flush loop
            // retries it like a merged tick.
            self.restore_pending(key, event).await;
        }
        result
    }

    /// Keeps a plain progress tick in memory.
    async fn defer(&self, key: ProgressKey, event: OwnedPlaybackEvent) {
        let observed_at = unix_timestamp();
        let evicted = {
            let mut state = self.state.lock().await;
            if state.pending.contains_key(&key) {
                if let Some(entry) = state.pending.get_mut(&key) {
                    entry.event = merge_into(event, &entry.event);
                    entry.observed_at = observed_at;
                }
                None
            } else {
                let evicted = if state.pending.len() >= self.max_pending {
                    oldest_pending(&state.pending)
                } else {
                    None
                };
                if let Some((evicted_key, _)) = evicted.as_ref() {
                    state.pending.remove(evicted_key);
                }
                state
                    .pending
                    .insert(key, PendingProgress::new(event, observed_at, self.window));
                evicted
            }
        };
        if let Some((evicted_key, evicted)) = evicted {
            // The layer is full, so the oldest session is written immediately instead of waiting
            // for its due time.
            if let Err(error) = self.flush_pending(evicted_key, evicted).await {
                tracing::error!(
                    event = "playback_progress_dropped",
                    %error,
                    "dropped merged playback progress: the merge layer is full and the write failed"
                );
            }
        }
        self.notify.notify_one();
    }

    async fn pending_entry(&self, user_id: &str, play_session_id: &str) -> Option<PendingProgress> {
        let state = self.state.lock().await;
        state
            .pending
            .get(&ProgressKey {
                user_id: user_id.to_owned(),
                play_session_id: play_session_id.to_owned(),
            })
            .cloned()
    }

    async fn pending_is_due(&self, key: &ProgressKey) -> bool {
        let state = self.state.lock().await;
        state
            .pending
            .get(key)
            .is_some_and(|entry| entry.due_at <= Instant::now())
    }

    async fn take_pending(&self, key: &ProgressKey) -> Option<PendingProgress> {
        self.state.lock().await.pending.remove(key)
    }

    async fn restore_pending(&self, key: ProgressKey, event: OwnedPlaybackEvent) {
        let mut state = self.state.lock().await;
        match state.pending.get_mut(&key) {
            Some(entry) => {
                entry.event = merge_into(entry.event.clone(), &event);
            }
            None => {
                let observed_at = unix_timestamp();
                let mut restored = PendingProgress::new(event, observed_at, self.window);
                restored.due_at = Instant::now() + FAILED_FLUSH_RETRY_DELAY;
                state.pending.insert(key, restored);
            }
        }
    }

    /// How long the flush loop may sleep before the earliest pending tick is due.
    async fn next_due_in(&self) -> Option<Duration> {
        let state = self.state.lock().await;
        let now = Instant::now();
        state
            .pending
            .values()
            .map(|entry| entry.due_at.saturating_duration_since(now))
            .min()
    }

    /// Writes every pending tick that is due.
    async fn flush_due(&self) {
        loop {
            let next = {
                let mut state = self.state.lock().await;
                let now = Instant::now();
                let key = state
                    .pending
                    .iter()
                    .filter(|(_, entry)| entry.due_at <= now)
                    .min_by_key(|(_, entry)| entry.due_at)
                    .map(|(key, _)| key.clone());
                key.and_then(|key| state.pending.remove(&key).map(|entry| (key, entry)))
            };
            let Some((key, entry)) = next else {
                return;
            };
            if let Err(error) = self.flush_pending(key.clone(), entry.clone()).await {
                tracing::warn!(
                    event = "playback_progress_flush_failed",
                    %error,
                    "failed to write merged playback progress; keeping it for the next pass"
                );
                self.restore_pending(key, entry.event).await;
            }
        }
    }

    /// Writes every pending tick, due or not.
    async fn flush_all(&self) {
        loop {
            let next = {
                let mut state = self.state.lock().await;
                let key = state.pending.keys().next().cloned();
                key.and_then(|key| state.pending.remove(&key).map(|entry| (key, entry)))
            };
            let Some((key, entry)) = next else {
                return;
            };
            if let Err(error) = self.flush_pending(key.clone(), entry.clone()).await {
                tracing::error!(
                    event = "playback_progress_flush_failed",
                    %error,
                    "failed to write merged playback progress before shutdown"
                );
            }
        }
    }

    /// Writes one merged tick as if it had been recorded when the client reported it.
    async fn flush_pending(
        &self,
        key: ProgressKey,
        entry: PendingProgress,
    ) -> Result<(), StorageError> {
        let persisted = self
            .database
            .find_playback_session(&key.user_id, &key.play_session_id)
            .await?;
        if let Some(persisted) = persisted.as_ref()
            && persisted.state == STOPPED_STATE
            && entry.event.state != STOPPED_STATE
            && persisted.last_event_at >= entry.observed_at
        {
            // The session was stopped after this tick was reported. Writing the merged tick last
            // would restart a session the stop already ended, so the stop wins, exactly as it does
            // in the uncoalesced order.
            return Ok(());
        }
        let effects = self
            .database
            .record_playback_event(entry.event.as_event(
                persisted.as_ref().map(|session| session.state.as_str()),
                persisted.as_ref().map(|session| session.position_ticks),
            ))
            .await?;
        if effects.is_played_changed
            && let Some(scope) = effects.container_scope.as_ref()
        {
            self.database
                .sync_played_container_states_for_scope(&key.user_id, scope)
                .await?;
        }
        Ok(())
    }
}

/// Folds `older` into `newer` with the merge rules of the session upsert.
fn merge_into(mut newer: OwnedPlaybackEvent, older: &OwnedPlaybackEvent) -> OwnedPlaybackEvent {
    newer.merge_older(older);
    newer
}

fn oldest_pending(
    pending: &HashMap<ProgressKey, PendingProgress>,
) -> Option<(ProgressKey, PendingProgress)> {
    pending
        .iter()
        .min_by_key(|(_, entry)| entry.due_at)
        .map(|(key, entry)| (key.clone(), entry.clone()))
}

/// Applies a pending tick to a stored session the way the write would have.
fn apply_pending(session: StoredPlaybackSession, entry: &PendingProgress) -> StoredPlaybackSession {
    let event = &entry.event;
    StoredPlaybackSession {
        item_id: event.item_id.clone(),
        media_source_id: event.media_source_id.clone().or(session.media_source_id),
        device_id: if event.device_id == UNKNOWN_DEVICE {
            session.device_id
        } else {
            event.device_id.clone()
        },
        client: event.client.clone().or(session.client),
        device_name: event.device_name.clone().or(session.device_name),
        client_version: event.client_version.clone().or(session.client_version),
        device_type: event.device_type.clone().or(session.device_type),
        remote_ip: event.remote_ip.clone().or(session.remote_ip),
        state: event.state.clone(),
        position_ticks: session.position_ticks.max(event.position_ticks),
        duration_ticks: event.duration_ticks.or(session.duration_ticks),
        is_paused: event.is_paused,
        last_event_at: entry.observed_at,
        ..session
    }
}

/// Builds the session a pending tick describes when its write has not created the row yet.
fn synthetic_session(entry: &PendingProgress) -> StoredPlaybackSession {
    let event = &entry.event;
    StoredPlaybackSession {
        id: String::new(),
        user_id: event.user_id.clone(),
        item_id: event.item_id.clone(),
        media_source_id: event.media_source_id.clone(),
        play_session_id: event.play_session_id.clone(),
        device_id: event.device_id.clone(),
        client: event.client.clone(),
        device_name: event.device_name.clone(),
        client_version: event.client_version.clone(),
        device_type: event.device_type.clone(),
        remote_ip: event.remote_ip.clone(),
        state: event.state.clone(),
        position_ticks: event.position_ticks,
        duration_ticks: event.duration_ticks,
        is_paused: event.is_paused,
        started_at: entry.observed_at,
        last_event_at: entry.observed_at,
    }
}

fn overlay_session(
    persisted: Option<StoredPlaybackSession>,
    pending: Option<&PendingProgress>,
) -> Option<StoredPlaybackSession> {
    match (persisted, pending) {
        (Some(session), Some(entry)) => Some(apply_pending(session, entry)),
        (Some(session), None) => Some(session),
        (None, Some(entry)) => Some(synthetic_session(entry)),
        (None, None) => None,
    }
}

fn unix_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// Sleeps until the earliest pending tick is due, then writes what is due.
///
/// Nothing pending means the loop waits for a notification instead of a timer, so an idle server
/// does not wake up for it. Shutdown drains everything still pending before the loop returns, which
/// is what makes the graceful stop lose no progress.
async fn run_flush_worker(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    loop {
        match shared.next_due_in().await {
            None => {
                tokio::select! {
                    () = shared.notify.notified() => {}
                    _ = shutdown.changed() => {
                        shared.flush_all().await;
                        return;
                    }
                }
            }
            Some(wait) => {
                let shutdown_requested = tokio::select! {
                    () = shared.notify.notified() => false,
                    requested = wait_or_shutdown(&mut shutdown, wait) => requested,
                };
                if shutdown_requested {
                    shared.flush_all().await;
                    return;
                }
                shared.flush_due().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{PLAYBACK_PROGRESS_FLUSH_WINDOW, PlaybackProgressService};
    use crate::{
        application::{libraries::LibraryService, setup::SetupService},
        library::LibraryKind,
        storage::{Database, NewPlaybackEvent, test_database},
    };

    const ITEM_ID: &str = "progress-movie";
    const PLAY_SESSION_ID: &str = "progress-play-session";
    const OTHER_PLAY_SESSION_ID: &str = "progress-other-session";
    const EPISODE_ID: &str = "progress-episode";
    const RUNTIME_TICKS: i64 = 10_000;

    struct Fixture {
        _temp: tempfile::TempDir,
        database: Database,
        user_id: String,
    }

    async fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
        let (temp, database) = test_database().await?;
        let user = SetupService::new(database.clone())?
            .complete("progress", "Progress", "correct password")
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Progress", LibraryKind::Movie, false)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status, runtime_ticks
             ) VALUES
                 (?, ?, 'MOVIE', 'Progress', 'progress', 'LOCAL_CONFIRMED', ?),
                 ('progress-series', ?, 'SERIES', 'Series', 'series', 'LOCAL_CONFIRMED', NULL),
                 ('progress-season', ?, 'SEASON', 'Season', 'season', 'LOCAL_CONFIRMED', NULL),
                 (?, ?, 'EPISODE', 'Episode', 'episode', 'LOCAL_CONFIRMED', ?)",
        )
        .bind(ITEM_ID)
        .bind(library.id.to_string())
        .bind(RUNTIME_TICKS)
        .bind(library.id.to_string())
        .bind(library.id.to_string())
        .bind(EPISODE_ID)
        .bind(library.id.to_string())
        .bind(RUNTIME_TICKS)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "UPDATE media_items SET parent_id = 'progress-season', series_id = 'progress-series',
                 season_number = 1, episode_number = 1
             WHERE id = ?",
        )
        .bind(EPISODE_ID)
        .execute(database.pool())
        .await?;
        // A resume threshold small enough that a test position can cross it.
        sqlx::query(
            "INSERT INTO server_settings (key, value) VALUES ('resume_min_ticks', '500')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "CREATE TABLE progress_write_log (table_name TEXT NOT NULL);
             CREATE TRIGGER progress_session_insert AFTER INSERT ON playback_sessions
             BEGIN INSERT INTO progress_write_log VALUES ('playback_sessions'); END;
             CREATE TRIGGER progress_session_update AFTER UPDATE ON playback_sessions
             BEGIN INSERT INTO progress_write_log VALUES ('playback_sessions'); END;
             CREATE TRIGGER progress_state_insert AFTER INSERT ON user_item_state
             BEGIN INSERT INTO progress_write_log VALUES ('user_item_state'); END;
             CREATE TRIGGER progress_state_update AFTER UPDATE ON user_item_state
             BEGIN INSERT INTO progress_write_log VALUES ('user_item_state'); END;",
        )
        .execute(database.pool())
        .await?;
        Ok(Fixture {
            _temp: temp,
            database,
            user_id: user.id.to_string(),
        })
    }

    impl Fixture {
        async fn writes(&self) -> Result<i64, Box<dyn std::error::Error>> {
            Ok(
                sqlx::query_scalar("SELECT COUNT(*) FROM progress_write_log")
                    .fetch_one(self.database.pool())
                    .await?,
            )
        }

        async fn reset_writes(&self) -> Result<(), Box<dyn std::error::Error>> {
            sqlx::query("DELETE FROM progress_write_log")
                .execute(self.database.pool())
                .await?;
            Ok(())
        }

        async fn stored_session(
            &self,
            play_session_id: &str,
        ) -> Result<(String, i64, Option<i64>, i64), Box<dyn std::error::Error>> {
            Ok(sqlx::query_as(
                "SELECT state, position_ticks, duration_ticks, is_paused
                 FROM playback_sessions WHERE user_id = ? AND play_session_id = ?",
            )
            .bind(&self.user_id)
            .bind(play_session_id)
            .fetch_one(self.database.pool())
            .await?)
        }

        async fn item_position(&self) -> Result<i64, Box<dyn std::error::Error>> {
            Ok(sqlx::query_scalar(
                "SELECT position_ticks FROM user_item_state WHERE user_id = ? AND item_id = ?",
            )
            .bind(&self.user_id)
            .bind(ITEM_ID)
            .fetch_one(self.database.pool())
            .await?)
        }

        /// Reports one tick the way a handler does: the previous value comes from the service.
        async fn tick(
            &self,
            service: &PlaybackProgressService,
            play_session_id: &str,
            item_id: &str,
            state: &str,
            position_ticks: i64,
        ) -> Result<(), Box<dyn std::error::Error>> {
            let previous = service.find_session(&self.user_id, play_session_id).await?;
            service
                .record(NewPlaybackEvent {
                    user_id: &self.user_id,
                    item_id,
                    media_source_id: None,
                    play_session_id,
                    device_id: "progress-device",
                    client: Some("Progress"),
                    device_name: Some("Progress"),
                    client_version: None,
                    device_type: Some("Progress"),
                    remote_ip: None,
                    state,
                    position_ticks,
                    duration_ticks: Some(RUNTIME_TICKS),
                    played_percent: 95,
                    is_paused: state == "PAUSED",
                    previous_state: previous.as_ref().map(|session| session.state.as_str()),
                    previous_position_ticks: previous
                        .as_ref()
                        .map(|session| session.position_ticks),
                    renew_admission: false,
                })
                .await?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn plain_progress_ticks_merge_into_one_write() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let service = PlaybackProgressService::new(fixture.database.clone());
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 100)
            .await?;
        fixture.reset_writes().await?;

        for position_ticks in [200, 300, 400, 500, 600] {
            fixture
                .tick(
                    &service,
                    PLAY_SESSION_ID,
                    ITEM_ID,
                    "PLAYING",
                    position_ticks,
                )
                .await?;
        }
        assert_eq!(fixture.writes().await?, 0, "plain ticks must not write");
        assert_eq!(service.pending_len().await, 1);
        assert_eq!(fixture.stored_session(PLAY_SESSION_ID).await?.1, 100);

        service.flush_all().await;
        assert_eq!(
            fixture.writes().await?,
            2,
            "one session and one item state write"
        );
        let (state, position_ticks, duration_ticks, is_paused) =
            fixture.stored_session(PLAY_SESSION_ID).await?;
        assert_eq!(state, "PLAYING");
        assert_eq!(position_ticks, 600, "the flush carries the newest position");
        assert_eq!(duration_ticks, Some(RUNTIME_TICKS));
        assert_eq!(is_paused, 0);
        assert_eq!(fixture.item_position().await?, 600);
        assert_eq!(service.pending_len().await, 0);
        Ok(())
    }

    #[tokio::test]
    async fn state_changes_are_written_immediately() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let service = PlaybackProgressService::new(fixture.database.clone());
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 100)
            .await?;
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 200)
            .await?;
        assert_eq!(service.pending_len().await, 1);

        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PAUSED", 250)
            .await?;
        assert_eq!(service.pending_len().await, 0);
        let (state, position_ticks, _, is_paused) = fixture.stored_session(PLAY_SESSION_ID).await?;
        assert_eq!(state, "PAUSED");
        assert_eq!(position_ticks, 250, "the pending position is folded in");
        assert_eq!(is_paused, 1);
        assert_eq!(fixture.item_position().await?, 250);
        Ok(())
    }

    #[tokio::test]
    async fn pending_progress_is_visible_to_session_reads() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = fixture().await?;
        let service = PlaybackProgressService::new(fixture.database.clone());
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 100)
            .await?;
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 700)
            .await?;

        let session = service
            .find_session(&fixture.user_id, PLAY_SESSION_ID)
            .await?
            .ok_or("pending session should be readable")?;
        assert_eq!(session.state, "PLAYING");
        assert_eq!(session.position_ticks, 700);
        assert_eq!(session.duration_ticks, Some(RUNTIME_TICKS));

        let listed = service.list_sessions(Some(&fixture.user_id), None).await?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].position_ticks, 700);

        let stored = fixture.stored_session(PLAY_SESSION_ID).await?;
        assert_eq!(stored.1, 100, "nothing was written yet");
        Ok(())
    }

    #[tokio::test]
    async fn a_tick_that_may_flip_played_is_written_immediately()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let service = PlaybackProgressService::new(fixture.database.clone());
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 100)
            .await?;
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 200)
            .await?;
        assert_eq!(service.pending_len().await, 1);

        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 9_600)
            .await?;
        assert_eq!(service.pending_len().await, 0);
        assert_eq!(fixture.stored_session(PLAY_SESSION_ID).await?.1, 9_600);
        let played: i64 = sqlx::query_scalar(
            "SELECT is_played FROM user_item_state WHERE user_id = ? AND item_id = ?",
        )
        .bind(&fixture.user_id)
        .bind(ITEM_ID)
        .fetch_one(fixture.database.pool())
        .await?;
        assert_eq!(played, 1);
        Ok(())
    }

    #[tokio::test]
    async fn a_tick_that_crosses_the_resume_threshold_is_written_immediately()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let service = PlaybackProgressService::new(fixture.database.clone());
        fixture
            .tick(&service, PLAY_SESSION_ID, EPISODE_ID, "PLAYING", 100)
            .await?;
        fixture
            .tick(&service, PLAY_SESSION_ID, EPISODE_ID, "PLAYING", 200)
            .await?;
        assert_eq!(service.pending_len().await, 1, "below the threshold");

        // The configured threshold is 500 ticks, so this tick crosses it.
        fixture
            .tick(&service, PLAY_SESSION_ID, EPISODE_ID, "PLAYING", 600)
            .await?;
        assert_eq!(service.pending_len().await, 0);
        assert_eq!(fixture.stored_session(PLAY_SESSION_ID).await?.1, 600);
        Ok(())
    }

    #[tokio::test]
    async fn the_flush_window_writes_merged_progress() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let service = PlaybackProgressService::with_limits(
            fixture.database.clone(),
            Duration::from_millis(40),
            16,
        );
        service.spawn().await;
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 100)
            .await?;
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 200)
            .await?;
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 300)
            .await?;
        fixture.reset_writes().await?;

        for _ in 0..100 {
            if fixture.stored_session(PLAY_SESSION_ID).await?.1 == 300 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(fixture.stored_session(PLAY_SESSION_ID).await?.1, 300);
        assert_eq!(
            fixture.writes().await?,
            2,
            "one merged write for the session and one for the item state"
        );
        assert_eq!(service.pending_len().await, 0);
        service.shutdown().await;
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_writes_pending_progress() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let service = PlaybackProgressService::new(fixture.database.clone());
        service.spawn().await;
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 100)
            .await?;
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 900)
            .await?;
        assert_eq!(service.pending_len().await, 1);

        service.shutdown().await;
        assert_eq!(service.pending_len().await, 0);
        assert_eq!(fixture.stored_session(PLAY_SESSION_ID).await?.1, 900);
        Ok(())
    }

    #[tokio::test]
    async fn the_merge_layer_bound_writes_the_oldest_session()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let service = PlaybackProgressService::with_limits(
            fixture.database.clone(),
            PLAYBACK_PROGRESS_FLUSH_WINDOW,
            1,
        );
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 100)
            .await?;
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 200)
            .await?;
        assert_eq!(service.pending_len().await, 1);

        fixture
            .tick(&service, OTHER_PLAY_SESSION_ID, ITEM_ID, "PLAYING", 100)
            .await?;
        fixture
            .tick(&service, OTHER_PLAY_SESSION_ID, ITEM_ID, "PLAYING", 400)
            .await?;
        assert_eq!(service.pending_len().await, 1);
        assert_eq!(
            fixture.stored_session(PLAY_SESSION_ID).await?.1,
            200,
            "the evicted session is written instead of dropped"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_stopped_session_is_not_restarted_by_a_pending_tick()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let service = PlaybackProgressService::new(fixture.database.clone());
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 100)
            .await?;
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 200)
            .await?;
        let session = fixture
            .database
            .find_playback_session(&fixture.user_id, PLAY_SESSION_ID)
            .await?
            .ok_or("session should exist")?;
        fixture
            .database
            .stop_playback_session(&session.id, unix_now())
            .await?;

        service.flush_all().await;
        let (state, position_ticks, _, _) = fixture.stored_session(PLAY_SESSION_ID).await?;
        assert_eq!(state, "STOPPED");
        assert_eq!(position_ticks, 100, "the stopped row keeps its position");
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_flush_keeps_the_pending_progress() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture().await?;
        let service = PlaybackProgressService::new(fixture.database.clone());
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 100)
            .await?;
        fixture
            .tick(&service, PLAY_SESSION_ID, ITEM_ID, "PLAYING", 500)
            .await?;
        sqlx::query("DROP TABLE playback_sessions")
            .execute(fixture.database.pool())
            .await?;

        service.flush_due().await;
        assert_eq!(service.pending_len().await, 1, "a failed write is kept");
        Ok(())
    }

    fn unix_now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or_default()
    }
}
