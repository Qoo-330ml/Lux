use tokio::sync::broadcast;

const EVENT_CHANNEL_CAPACITY: usize = 256;

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

#[cfg(test)]
mod tests {
    use super::{AdminEventHub, AdminEventScope};

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
    }
}
