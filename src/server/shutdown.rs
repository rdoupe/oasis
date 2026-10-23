//! Why the server is stopping, recorded where the request arrives and logged
//! once when shutdown begins.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::platform::ServerQuitSignal;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShutdownReason {
    Signal(ServerQuitSignal),
    ApiStop { caller: Option<String> },
    HostShutdown,
    Unknown,
}

impl std::fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Signal(signal) => write!(f, "{signal}"),
            Self::ApiStop {
                caller: Some(caller),
            } => write!(f, "server.stop request from {caller}"),
            Self::ApiStop { caller: None } => f.write_str("server.stop request"),
            Self::HostShutdown => f.write_str("host shutdown"),
            Self::Unknown => f.write_str("unknown"),
        }
    }
}

/// The server's stop flag plus the reason recorded by the first stop request.
#[derive(Clone, Default)]
pub(crate) struct ServerStop {
    flag: Arc<AtomicBool>,
    reason: Arc<Mutex<Option<ShutdownReason>>>,
    /// Wakes an idle server loop, which no longer polls the flag on a timer.
    requested: Arc<Notify>,
}

impl ServerStop {
    pub(crate) fn flag(&self) -> &Arc<AtomicBool> {
        &self.flag
    }

    pub(crate) fn is_requested(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    pub(crate) fn request(&self, reason: ShutdownReason) {
        if let Ok(mut recorded) = self.reason.lock() {
            recorded.get_or_insert(reason);
        }
        self.flag.store(true, Ordering::Release);
        self.requested.notify_one();
    }

    /// Resolves once a stop has been requested.
    pub(crate) async fn wait_requested(&self) {
        while !self.is_requested() {
            self.requested.notified().await;
        }
    }

    pub(crate) fn take_reason(&self) -> Option<ShutdownReason> {
        self.reason.lock().ok()?.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_stop_reason_wins() {
        let stop = ServerStop::default();
        assert!(!stop.is_requested());

        stop.request(ShutdownReason::HostShutdown);
        stop.request(ShutdownReason::ApiStop { caller: None });

        assert!(stop.is_requested());
        assert_eq!(stop.take_reason(), Some(ShutdownReason::HostShutdown));
        assert_eq!(stop.take_reason(), None);
    }

    #[test]
    fn api_stop_reason_names_the_caller() {
        let reason = ShutdownReason::ApiStop {
            caller: Some("pid 42 (herdr), parent pid 7 (zsh)".into()),
        };
        assert_eq!(
            reason.to_string(),
            "server.stop request from pid 42 (herdr), parent pid 7 (zsh)"
        );
    }

    #[tokio::test]
    async fn stop_request_from_another_thread_wakes_a_waiting_loop() {
        let stop = ServerStop::default();
        let requester = stop.clone();
        std::thread::spawn(move || requester.request(ShutdownReason::ApiStop { caller: None }));

        tokio::time::timeout(std::time::Duration::from_secs(5), stop.wait_requested())
            .await
            .expect("stop request wakes the waiter");
        assert!(stop.is_requested());
    }
}
