//! The shutdown broadcast every open connection watches.

use tokio::sync::watch;

/// The shutdown broadcast every open connection watches.
#[derive(Clone)]
pub(crate) struct WebsocketShutdown {
    sender: watch::Sender<bool>,
}

impl Default for WebsocketShutdown {
    fn default() -> Self {
        Self {
            sender: watch::channel(false).0,
        }
    }
}

impl WebsocketShutdown {
    pub(crate) fn signal(&self) {
        self.sender.send_replace(true);
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<bool> {
        self.sender.subscribe()
    }
}
