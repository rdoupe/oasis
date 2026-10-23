use std::io;
use std::sync::{atomic::AtomicBool, atomic::Ordering, Arc};

use interprocess::local_socket::traits::{Listener as _, Stream as _};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use crate::ipc::LocalListener;
use crate::server::client_transport::{self, ServerEvent};

/// Accept readiness of the non-blocking client listener, so an idle server loop
/// sleeps until a client connects instead of polling the listener.
#[cfg(unix)]
#[derive(Default)]
pub(crate) struct ClientAcceptReadiness {
    registered: Option<tokio::io::unix::AsyncFd<std::os::fd::RawFd>>,
    registration_failed: bool,
}

#[cfg(unix)]
impl ClientAcceptReadiness {
    /// Registers the listener with the runtime's reactor. Must run inside the
    /// runtime; a failure leaves the caller on its polling fallback.
    pub(crate) fn register(&mut self, listener: &LocalListener) {
        if self.registered.is_some() || self.registration_failed {
            return;
        }
        match tokio::io::unix::AsyncFd::with_interest(
            crate::platform::local_listener_raw_fd(listener),
            tokio::io::Interest::READABLE,
        ) {
            Ok(fd) => self.registered = Some(fd),
            Err(err) => {
                warn!(err = %err, "client listener readiness unavailable; polling for clients");
                self.registration_failed = true;
            }
        }
    }

    /// Drops the registration. Call before the listener is replaced, while the
    /// old descriptor is still open.
    pub(crate) fn reset(&mut self) {
        self.registered = None;
        self.registration_failed = false;
    }

    pub(crate) fn is_registered(&self) -> bool {
        self.registered.is_some()
    }

    /// Resolves once a connection is pending. Readiness is cleared before
    /// returning, so the accept loop must drain the listener until `WouldBlock`.
    pub(crate) async fn wait(&mut self) {
        let Some(fd) = self.registered.as_ref() else {
            return std::future::pending().await;
        };
        match fd.readable().await {
            Ok(mut guard) => guard.clear_ready(),
            Err(err) => {
                warn!(err = %err, "client listener readiness failed; polling for clients");
                self.registered = None;
                self.registration_failed = true;
            }
        }
    }
}

/// Accepts pending thin-client connections and starts their handshake readers.
pub(crate) fn accept_pending_client_connections(
    listener: &LocalListener,
    next_client_id: &mut u64,
    should_quit: &Arc<AtomicBool>,
    server_event_tx: &mpsc::Sender<ServerEvent>,
) -> io::Result<()> {
    loop {
        if should_quit.load(Ordering::Acquire) {
            break;
        }
        match listener.accept() {
            Ok(stream) => {
                let client_id = *next_client_id;
                *next_client_id = next_client_id.saturating_add(1);

                if let Err(err) = stream.set_nonblocking(true) {
                    warn!(err = %err, "failed to set client stream nonblocking");
                    continue;
                }

                let should_quit = should_quit.clone();
                let server_event_tx = server_event_tx.clone();
                let spawned = crate::thread_spawn::spawn_named("herdr-client-conn", move || {
                    if let Err(err) = client_transport::handle_client_handshake(
                        stream,
                        client_id,
                        &server_event_tx,
                        &should_quit,
                    ) {
                        debug!(client_id, err = %err, "client handshake failed");
                    }
                });
                if let Err(err) = spawned {
                    warn!(client_id, err = %err, "failed to spawn client connection thread; dropping connection");
                }
            }
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) => {
                error!(err = %err, "client listener accept failed");
                break;
            }
        }
    }

    Ok(())
}

/// Drains pending thin-client connections without starting handshakes.
///
/// During live handoff the old server must not let clients sit in the Unix
/// listener backlog waiting for a welcome frame that will never be sent.
pub(crate) fn reject_pending_client_connections(listener: &LocalListener) -> io::Result<()> {
    loop {
        match listener.accept() {
            Ok(_stream) => {}
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => break,
            Err(err) => {
                error!(err = %err, "client listener reject failed");
                break;
            }
        }
    }

    Ok(())
}
