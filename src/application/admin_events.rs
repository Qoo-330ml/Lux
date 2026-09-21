use std::{sync::Arc, time::Duration};

use tokio::sync::{Mutex, broadcast};

const EVENT_CHANNEL_CAPACITY: usize = 256;
const HOME_EVENT_COALESCE_WINDOW: Duration = Duration::from_secs(1);

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UserEventScope {
    Home,
}

impl UserEventScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
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
    sender: broadcast::Sender<AdminEventScope>,
}

#[derive(Clone)]
pub struct UserEventHub {
    sender: broadcast::Sender<UserEventScope>,
    home_event_state: Arc<Mutex<HomeEventState>>,
}

#[derive(Default)]
struct HomeEventState {
    scheduled: bool,
    dirty: bool,
    epoch: u64,
}

impl AdminEventHub {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self { sender }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AdminEventScope> {
        self.sender.subscribe()
    }

    pub fn publish(&self, scope: AdminEventScope) {
        let _ = self.sender.send(scope);
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
            home_event_state: Arc::new(Mutex::new(HomeEventState::default())),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<UserEventScope> {
        self.sender.subscribe()
    }

    pub fn publish(&self, scope: UserEventScope) {
        let _ = self.sender.send(scope);
    }

    pub async fn publish_home_coalesced(&self) {
        let epoch = {
            let mut state = self.home_event_state.lock().await;
            if state.scheduled {
                state.dirty = true;
                return;
            }
            state.scheduled = true;
            state.dirty = false;
            state.epoch = state.epoch.wrapping_add(1);
            state.epoch
        };

        let _ = self.sender.send(UserEventScope::Home);
        let sender = self.sender.clone();
        let state = Arc::clone(&self.home_event_state);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(HOME_EVENT_COALESCE_WINDOW).await;
                let send_trailing = {
                    let mut state = state.lock().await;
                    if state.epoch != epoch || !state.scheduled {
                        return;
                    }
                    if state.dirty {
                        state.dirty = false;
                        true
                    } else {
                        state.scheduled = false;
                        false
                    }
                };
                if !send_trailing {
                    return;
                }
                let _ = sender.send(UserEventScope::Home);
            }
        });
    }

    pub async fn publish_home_now(&self) {
        let mut state = self.home_event_state.lock().await;
        state.scheduled = false;
        state.dirty = false;
        state.epoch = state.epoch.wrapping_add(1);
        drop(state);
        let _ = self.sender.send(UserEventScope::Home);
    }
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

    use super::{AdminEventHub, AdminEventScope, UserEventHub, UserEventScope};

    #[tokio::test]
    async fn publishes_scopes_to_all_subscribers() {
        let hub = AdminEventHub::new();
        let mut first = hub.subscribe();
        let mut second = hub.subscribe();

        hub.publish(AdminEventScope::Jobs);

        assert_eq!(first.recv().await, Ok(AdminEventScope::Jobs));
        assert_eq!(second.recv().await, Ok(AdminEventScope::Jobs));
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
    }

    #[tokio::test]
    async fn publishes_user_scopes_to_all_subscribers() {
        let hub = UserEventHub::new();
        let mut first = hub.subscribe();
        let mut second = hub.subscribe();

        hub.publish(UserEventScope::Home);

        assert_eq!(first.recv().await, Ok(UserEventScope::Home));
        assert_eq!(second.recv().await, Ok(UserEventScope::Home));
    }

    #[tokio::test]
    async fn coalesces_home_events_and_flushes_immediately() {
        let hub = UserEventHub::new();
        let mut receiver = hub.subscribe();

        hub.publish_home_coalesced().await;
        assert_eq!(receiver.recv().await, Ok(UserEventScope::Home));

        hub.publish_home_coalesced().await;
        assert!(
            timeout(Duration::from_millis(25), receiver.recv())
                .await
                .is_err()
        );

        tokio::time::sleep(Duration::from_secs(1) + Duration::from_millis(25)).await;
        assert_eq!(receiver.recv().await, Ok(UserEventScope::Home));

        hub.publish_home_now().await;
        assert_eq!(receiver.recv().await, Ok(UserEventScope::Home));

        tokio::time::sleep(Duration::from_secs(1) + Duration::from_millis(25)).await;
        assert!(
            timeout(Duration::from_millis(25), receiver.recv())
                .await
                .is_err()
        );
    }
}
