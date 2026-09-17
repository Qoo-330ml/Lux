use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShutdownReason {
    Running,
    Shutdown,
    Restart,
}

#[derive(Clone, Debug)]
pub struct RestartHandle {
    sender: watch::Sender<ShutdownReason>,
}

impl RestartHandle {
    pub fn new(sender: watch::Sender<ShutdownReason>) -> Self {
        Self { sender }
    }

    pub fn request_restart(&self) -> bool {
        self.sender.send_if_modified(|reason| {
            if *reason != ShutdownReason::Running {
                return false;
            }
            *reason = ShutdownReason::Restart;
            true
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{RestartHandle, ShutdownReason};
    use tokio::sync::watch;

    #[test]
    fn restart_can_only_be_requested_once() {
        let (sender, receiver) = watch::channel(ShutdownReason::Running);
        let handle = RestartHandle::new(sender);

        assert!(handle.request_restart());
        assert!(!handle.request_restart());
        assert_eq!(*receiver.borrow(), ShutdownReason::Restart);
    }
}
