use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use tokio::sync::{Mutex, broadcast};

pub(crate) const EVENT_CHANNEL_CAPACITY: usize = 256;
const HOME_EVENT_COALESCE_WINDOW: Duration = Duration::from_secs(1);
const ADMIN_JOBS_EVENT_COALESCE_WINDOW: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdminEventScope {
    All,
    Dashboard,
    Jobs,
    Libraries,
    Plugins,
    Users,
    Metadata,
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum UserEventScope {
    Home,
    Item,
    Library,
    Favorites,
    /// Playback position updates for a single item, emitted only to the user
    /// who is reporting progress.
    Progress,
}

impl UserEventScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Item => "item",
            Self::Library => "library",
            Self::Favorites => "favorites",
            Self::Progress => "progress",
        }
    }
}

impl AdminEventScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Dashboard => "dashboard",
            Self::Jobs => "jobs",
            Self::Libraries => "libraries",
            Self::Plugins => "plugins",
            Self::Users => "users",
            Self::Metadata => "metadata",
            Self::Settings => "settings",
        }
    }
}

#[derive(Clone)]
pub struct AdminEventHub {
    user_events: Option<UserEventHub>,
    sender: broadcast::Sender<AdminEventScope>,
    jobs_event_state: Arc<StdMutex<AdminJobsEventState>>,
}

/// A user-facing invalidation notification.
///
/// Events without a `user_id` are directory-level notifications (scans,
/// metadata changes, administrator edits) that every subscriber may observe.
/// Events with a `user_id` describe one user's own state (playback progress,
/// favorites, watched flags, preferences) and are filtered out for every other
/// subscription before anything is written to the wire.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UserEvent {
    scope: UserEventScope,
    user_id: Option<String>,
    item_id: Option<String>,
}

impl UserEvent {
    /// A directory-level notification that reaches every subscriber.
    pub fn broadcast(scope: UserEventScope) -> Self {
        Self {
            scope,
            user_id: None,
            item_id: None,
        }
    }

    /// A notification about `user_id`'s own state.
    pub fn for_user(scope: UserEventScope, user_id: impl Into<String>) -> Self {
        Self {
            scope,
            user_id: Some(user_id.into()),
            item_id: None,
        }
    }

    /// Narrows a user-level notification to the item it is about.
    pub fn with_item_id(mut self, item_id: impl Into<String>) -> Self {
        self.item_id = Some(item_id.into());
        self
    }

    pub const fn scope(&self) -> UserEventScope {
        self.scope
    }

    pub fn item_id(&self) -> Option<&str> {
        self.item_id.as_deref()
    }

    /// Directory-level events are visible to everybody; user-level events are
    /// visible only to subscriptions opened for the same user.
    pub fn is_visible_to(&self, user_id: &str) -> bool {
        match self.user_id.as_deref() {
            None => true,
            Some(target) => target == user_id,
        }
    }
}

#[derive(Clone)]
pub struct UserEventHub {
    sender: broadcast::Sender<UserEvent>,
    coalesced_events: Arc<Mutex<CoalesceState>>,
}

/// A user's view of the event hub: broadcast events plus that user's own events.
///
/// Subscriptions are only created for a concrete user, so there is no way to
/// receive another user's user-level notifications.
pub struct UserEventSubscriber {
    user_id: String,
    receiver: broadcast::Receiver<UserEvent>,
}

impl UserEventSubscriber {
    /// Receives the next event visible to this subscriber.
    pub async fn recv(&mut self) -> Result<UserEvent, broadcast::error::RecvError> {
        loop {
            match self.receiver.recv().await {
                Ok(event) if event.is_visible_to(&self.user_id) => return Ok(event),
                Ok(_) => continue,
                Err(error) => return Err(error),
            }
        }
    }

    /// Receives the next already-buffered event visible to this subscriber.
    pub fn try_recv(&mut self) -> Result<UserEvent, broadcast::error::TryRecvError> {
        loop {
            match self.receiver.try_recv() {
                Ok(event) if event.is_visible_to(&self.user_id) => return Ok(event),
                Ok(_) => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

/// Coalescing is tracked per `(user, scope)` so one user's burst can never
/// swallow another user's pending notification.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct CoalesceKey {
    scope: UserEventScope,
    user_id: Option<String>,
}

#[derive(Default)]
struct CoalesceState {
    keys: HashMap<CoalesceKey, CoalesceEntry>,
}

#[derive(Default)]
struct CoalesceEntry {
    scheduled: bool,
    dirty: bool,
    epoch: u64,
    item_id: Option<String>,
}

#[derive(Default)]
struct AdminJobsEventState {
    scheduled: bool,
    dirty: bool,
    epoch: u64,
}

impl AdminEventHub {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            user_events: None,
            sender,
            jobs_event_state: Arc::new(StdMutex::new(AdminJobsEventState::default())),
        }
    }

    pub fn with_user_events(mut self, user_events: UserEventHub) -> Self {
        self.user_events = Some(user_events);
        self
    }

    fn publish_user_invalidation(&self, scope: AdminEventScope) {
        let Some(user_events) = &self.user_events else {
            return;
        };
        let scope = match scope {
            AdminEventScope::All | AdminEventScope::Metadata | AdminEventScope::Jobs => {
                UserEventScope::Item
            }
            AdminEventScope::Libraries => UserEventScope::Library,
            AdminEventScope::Users | AdminEventScope::Settings => UserEventScope::Home,
            AdminEventScope::Dashboard | AdminEventScope::Plugins => return,
        };
        user_events.publish(scope);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AdminEventScope> {
        self.sender.subscribe()
    }

    pub fn publish(&self, scope: AdminEventScope) {
        if scope == AdminEventScope::Jobs {
            self.publish_jobs_now();
            return;
        }
        let _ = self.sender.send(scope);
        self.publish_user_invalidation(scope);
    }

    pub fn publish_jobs_progress(&self) {
        let Ok(mut state) = self.jobs_event_state.lock() else {
            let _ = self.sender.send(AdminEventScope::Jobs);
            return;
        };
        if state.scheduled {
            state.dirty = true;
            return;
        }

        state.scheduled = true;
        state.dirty = false;
        state.epoch = state.epoch.wrapping_add(1);
        let epoch = state.epoch;
        let _ = self.sender.send(AdminEventScope::Jobs);

        let sender = self.sender.clone();
        let event_state = Arc::clone(&self.jobs_event_state);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(ADMIN_JOBS_EVENT_COALESCE_WINDOW).await;
                let Ok(mut state) = event_state.lock() else {
                    return;
                };
                if state.epoch != epoch || !state.scheduled {
                    return;
                }
                if state.dirty {
                    state.dirty = false;
                    let _ = sender.send(AdminEventScope::Jobs);
                    continue;
                }
                state.scheduled = false;
                return;
            }
        });
    }

    pub fn publish_jobs_now(&self) {
        self.publish_user_invalidation(AdminEventScope::Jobs);
        if let Ok(mut state) = self.jobs_event_state.lock() {
            state.scheduled = false;
            state.dirty = false;
            state.epoch = state.epoch.wrapping_add(1);
        }
        let _ = self.sender.send(AdminEventScope::Jobs);
    }
}

impl Default for AdminEventHub {
    fn default() -> Self {
        Self::new()
    }
}

impl UserEventHub {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            sender,
            coalesced_events: Arc::new(Mutex::new(CoalesceState::default())),
        }
    }

    /// Subscribes to the events `user_id` is allowed to observe.
    pub fn subscribe_for(&self, user_id: &str) -> UserEventSubscriber {
        UserEventSubscriber {
            user_id: user_id.to_owned(),
            receiver: self.sender.subscribe(),
        }
    }

    /// Publishes a directory-level notification to every subscriber.
    pub fn publish(&self, scope: UserEventScope) {
        let _ = self.sender.send(UserEvent::broadcast(scope));
    }

    /// Publishes a notification about one user's own state.
    pub fn publish_for_user(&self, scope: UserEventScope, user_id: &str) {
        let _ = self.sender.send(UserEvent::for_user(scope, user_id));
    }

    /// Publishes a coalesced directory-level home invalidation.
    pub async fn publish_home_coalesced(&self) {
        self.publish_coalesced(
            UserEventScope::Home,
            UserEvent::broadcast(UserEventScope::Home),
        )
        .await;
    }

    /// Publishes a coalesced home invalidation for one user only.
    pub async fn publish_home_coalesced_for(&self, user_id: &str) {
        self.publish_coalesced(
            UserEventScope::Home,
            UserEvent::for_user(UserEventScope::Home, user_id),
        )
        .await;
    }

    /// Publishes a coalesced playback progress invalidation for one user.
    pub async fn publish_progress_coalesced(&self, user_id: &str, item_id: &str) {
        self.publish_coalesced(
            UserEventScope::Progress,
            UserEvent::for_user(UserEventScope::Progress, user_id).with_item_id(item_id),
        )
        .await;
    }

    pub async fn publish_home_now(&self) {
        let mut state = self.coalesced_events.lock().await;
        if let Some(entry) = state.keys.get_mut(&CoalesceKey {
            scope: UserEventScope::Home,
            user_id: None,
        }) {
            entry.scheduled = false;
            entry.dirty = false;
            entry.epoch = entry.epoch.wrapping_add(1);
        }
        drop(state);
        self.publish(UserEventScope::Home);
    }

    async fn publish_coalesced(&self, scope: UserEventScope, event: UserEvent) {
        let key = CoalesceKey {
            scope,
            user_id: event.user_id.clone(),
        };
        let epoch = {
            let mut state = self.coalesced_events.lock().await;
            let entry = state.keys.entry(key.clone()).or_default();
            entry.item_id = event.item_id.clone();
            if entry.scheduled {
                entry.dirty = true;
                return;
            }
            entry.scheduled = true;
            entry.dirty = false;
            entry.epoch = entry.epoch.wrapping_add(1);
            entry.epoch
        };

        let _ = self.sender.send(event);
        let sender = self.sender.clone();
        let state = Arc::clone(&self.coalesced_events);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(HOME_EVENT_COALESCE_WINDOW).await;
                let flush = {
                    let mut state = state.lock().await;
                    match state.keys.get_mut(&key) {
                        Some(entry) if entry.epoch == epoch && entry.scheduled => {
                            if entry.dirty {
                                entry.dirty = false;
                                CoalesceFlush::Trailing(entry.item_id.clone())
                            } else {
                                entry.scheduled = false;
                                CoalesceFlush::Idle
                            }
                        }
                        _ => return,
                    }
                };
                let item_id = match flush {
                    CoalesceFlush::Trailing(item_id) => item_id,
                    CoalesceFlush::Idle => {
                        // The window closed with nothing pending: drop the bookkeeping
                        // entry so the table stays bounded by users who are active now.
                        let mut state = state.lock().await;
                        let idle = state.keys.get(&key).is_some_and(|entry| {
                            entry.epoch == epoch && !entry.scheduled && !entry.dirty
                        });
                        if idle {
                            state.keys.remove(&key);
                        }
                        return;
                    }
                };
                let mut event = match &key.user_id {
                    Some(user_id) => UserEvent::for_user(key.scope, user_id.clone()),
                    None => UserEvent::broadcast(key.scope),
                };
                if let Some(item_id) = item_id {
                    event = event.with_item_id(item_id);
                }
                let _ = sender.send(event);
            }
        });
    }

    /// Number of `(user, scope)` coalescing buckets currently tracked.
    #[cfg(test)]
    pub(crate) async fn tracked_coalesce_keys(&self) -> usize {
        self.coalesced_events.lock().await.keys.len()
    }
}

enum CoalesceFlush {
    Trailing(Option<String>),
    Idle,
}

impl Default for UserEventHub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::timeout;

    use super::{
        AdminEventHub, AdminEventScope, UserEventHub, UserEventScope, UserEventSubscriber,
    };

    async fn next_scope(subscriber: &mut UserEventSubscriber) -> Option<UserEventScope> {
        subscriber.recv().await.ok().map(|event| event.scope())
    }

    #[tokio::test]
    async fn committed_admin_changes_refresh_user_surfaces_without_progress_noise() {
        let user_events = UserEventHub::new();
        let mut receiver = user_events.subscribe_for("viewer");
        let hub = AdminEventHub::new().with_user_events(user_events);

        hub.publish_jobs_progress();
        hub.publish(AdminEventScope::Dashboard);
        hub.publish(AdminEventScope::Plugins);
        assert!(receiver.try_recv().is_err());

        for (admin_scope, user_scope) in [
            (AdminEventScope::Metadata, UserEventScope::Item),
            (AdminEventScope::Jobs, UserEventScope::Item),
            (AdminEventScope::Libraries, UserEventScope::Library),
            (AdminEventScope::Users, UserEventScope::Home),
            (AdminEventScope::Settings, UserEventScope::Home),
            (AdminEventScope::All, UserEventScope::Item),
        ] {
            hub.publish(admin_scope);
            assert_eq!(
                receiver.try_recv().ok().map(|event| event.scope()),
                Some(user_scope)
            );
        }
    }

    #[tokio::test]
    async fn publishes_scopes_to_all_subscribers() {
        let hub = AdminEventHub::new();
        let mut first = hub.subscribe();
        let mut second = hub.subscribe();

        hub.publish(AdminEventScope::Jobs);

        assert_eq!(first.recv().await, Ok(AdminEventScope::Jobs));
        assert_eq!(second.recv().await, Ok(AdminEventScope::Jobs));
    }

    #[tokio::test]
    async fn coalesces_jobs_progress_and_sends_one_trailing_refresh() {
        let hub = AdminEventHub::new();
        let mut receiver = hub.subscribe();

        hub.publish_jobs_progress();
        assert_eq!(
            timeout(Duration::from_secs(2), receiver.recv()).await,
            Ok(Ok(AdminEventScope::Jobs))
        );

        hub.publish_jobs_progress();
        hub.publish_jobs_progress();
        assert!(
            timeout(Duration::from_millis(25), receiver.recv())
                .await
                .is_err()
        );

        assert_eq!(
            timeout(Duration::from_secs(2), receiver.recv()).await,
            Ok(Ok(AdminEventScope::Jobs))
        );
        assert!(
            timeout(Duration::from_millis(25), receiver.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn immediate_jobs_refresh_cancels_pending_progress_trailing_event() {
        let hub = AdminEventHub::new();
        let mut receiver = hub.subscribe();

        hub.publish_jobs_progress();
        assert_eq!(receiver.recv().await, Ok(AdminEventScope::Jobs));
        hub.publish_jobs_progress();
        hub.publish(AdminEventScope::Jobs);
        assert_eq!(receiver.recv().await, Ok(AdminEventScope::Jobs));

        assert!(
            timeout(
                Duration::from_secs(1) + Duration::from_millis(25),
                receiver.recv()
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn scopes_use_stable_wire_names() {
        assert_eq!(AdminEventScope::All.as_str(), "all");
        assert_eq!(AdminEventScope::Dashboard.as_str(), "dashboard");
        assert_eq!(AdminEventScope::Jobs.as_str(), "jobs");
        assert_eq!(AdminEventScope::Libraries.as_str(), "libraries");
        assert_eq!(AdminEventScope::Plugins.as_str(), "plugins");
        assert_eq!(AdminEventScope::Users.as_str(), "users");
        assert_eq!(AdminEventScope::Metadata.as_str(), "metadata");
        assert_eq!(AdminEventScope::Settings.as_str(), "settings");
        assert_eq!(UserEventScope::Home.as_str(), "home");
        assert_eq!(UserEventScope::Item.as_str(), "item");
        assert_eq!(UserEventScope::Library.as_str(), "library");
        assert_eq!(UserEventScope::Favorites.as_str(), "favorites");
        assert_eq!(UserEventScope::Progress.as_str(), "progress");
    }

    #[tokio::test]
    async fn publishes_user_scopes_to_all_subscribers() {
        let hub = UserEventHub::new();
        let mut first = hub.subscribe_for("alice");
        let mut second = hub.subscribe_for("bob");

        hub.publish(UserEventScope::Home);

        assert_eq!(next_scope(&mut first).await, Some(UserEventScope::Home));
        assert_eq!(next_scope(&mut second).await, Some(UserEventScope::Home));
    }

    #[tokio::test]
    async fn user_scoped_events_only_reach_the_matching_user() {
        let hub = UserEventHub::new();
        let mut alice = hub.subscribe_for("alice");
        let mut bob = hub.subscribe_for("bob");

        hub.publish_for_user(UserEventScope::Favorites, "alice");

        assert_eq!(
            next_scope(&mut alice).await,
            Some(UserEventScope::Favorites)
        );
        assert!(
            bob.try_recv().is_err(),
            "another user's event must not reach this subscription"
        );

        // Directory-level notifications still reach every subscriber.
        hub.publish(UserEventScope::Item);
        assert_eq!(next_scope(&mut alice).await, Some(UserEventScope::Item));
        assert_eq!(next_scope(&mut bob).await, Some(UserEventScope::Item));
    }

    #[tokio::test]
    async fn coalesced_progress_events_stay_isolated_per_user_and_carry_the_item() {
        let hub = UserEventHub::new();
        let mut alice = hub.subscribe_for("alice");
        let mut bob = hub.subscribe_for("bob");

        hub.publish_progress_coalesced("alice", "item-1").await;
        let event = alice.recv().await.expect("alice progress event");
        assert_eq!(event.scope(), UserEventScope::Progress);
        assert_eq!(event.item_id(), Some("item-1"));
        assert!(bob.try_recv().is_err());

        // A second tick inside the window is coalesced into one trailing event.
        hub.publish_progress_coalesced("alice", "item-1").await;
        assert!(alice.try_recv().is_err());
        assert!(bob.try_recv().is_err());
        tokio::time::sleep(Duration::from_secs(1) + Duration::from_millis(25)).await;
        assert_eq!(next_scope(&mut alice).await, Some(UserEventScope::Progress));
        assert!(bob.try_recv().is_err());
    }

    #[tokio::test]
    async fn coalesced_home_events_are_tracked_per_user() {
        let hub = UserEventHub::new();
        let mut alice = hub.subscribe_for("alice");
        let mut bob = hub.subscribe_for("bob");

        hub.publish_home_coalesced_for("alice").await;
        // Bob publishes inside alice's coalescing window; his event must not be
        // swallowed by alice's pending trailing flush.
        hub.publish_home_coalesced_for("bob").await;
        assert_eq!(next_scope(&mut alice).await, Some(UserEventScope::Home));
        assert_eq!(next_scope(&mut bob).await, Some(UserEventScope::Home));

        hub.publish_home_coalesced_for("alice").await;
        assert!(alice.try_recv().is_err());
        assert!(bob.try_recv().is_err());
        tokio::time::sleep(Duration::from_secs(1) + Duration::from_millis(25)).await;
        assert_eq!(next_scope(&mut alice).await, Some(UserEventScope::Home));
        assert!(bob.try_recv().is_err());
    }

    #[tokio::test]
    async fn drops_idle_coalescing_buckets_after_the_window() {
        let hub = UserEventHub::new();
        let mut alice = hub.subscribe_for("alice");

        hub.publish_home_coalesced_for("alice").await;
        assert_eq!(next_scope(&mut alice).await, Some(UserEventScope::Home));
        assert_eq!(hub.tracked_coalesce_keys().await, 1);

        tokio::time::sleep(Duration::from_secs(1) + Duration::from_millis(25)).await;
        assert_eq!(
            hub.tracked_coalesce_keys().await,
            0,
            "an idle bucket must not stay in the coalescing table"
        );

        // A later publish starts a fresh bucket and still sends immediately.
        hub.publish_home_coalesced_for("alice").await;
        assert_eq!(next_scope(&mut alice).await, Some(UserEventScope::Home));
        tokio::time::sleep(Duration::from_secs(1) + Duration::from_millis(25)).await;
        assert_eq!(hub.tracked_coalesce_keys().await, 0);
    }

    #[tokio::test]
    async fn lagging_subscribers_observe_a_lagged_receiver() {
        let hub = UserEventHub::new();
        let mut subscriber = hub.subscribe_for("alice");
        for _ in 0..(super::EVENT_CHANNEL_CAPACITY + 8) {
            hub.publish(UserEventScope::Item);
        }
        assert!(matches!(
            subscriber.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_))
        ));
    }

    #[tokio::test]
    async fn coalesces_home_events_and_flushes_immediately() {
        let hub = UserEventHub::new();
        let mut receiver = hub.subscribe_for("viewer");

        hub.publish_home_coalesced().await;
        assert_eq!(next_scope(&mut receiver).await, Some(UserEventScope::Home));

        hub.publish_home_coalesced().await;
        assert!(
            timeout(Duration::from_millis(25), receiver.recv())
                .await
                .is_err()
        );

        tokio::time::sleep(Duration::from_secs(1) + Duration::from_millis(25)).await;
        assert_eq!(next_scope(&mut receiver).await, Some(UserEventScope::Home));

        hub.publish_home_now().await;
        assert_eq!(next_scope(&mut receiver).await, Some(UserEventScope::Home));

        tokio::time::sleep(Duration::from_secs(1) + Duration::from_millis(25)).await;
        assert!(
            timeout(Duration::from_millis(25), receiver.recv())
                .await
                .is_err()
        );
    }
}
