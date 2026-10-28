//! Input for a machine whose connection has stopped answering. It waits here
//! instead of going into the stalled transport, where a reconnect would discard
//! it, and goes out in order once the machine answers again.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::endpoint::ClientEndpointId;
use crate::protocol::ClientMessage;

/// More than this and the oldest input is dropped: a stuck key must not grow
/// without bound.
const MAX_HELD: usize = 512;
/// Input older than this is dropped rather than typed long after the person
/// moved on.
const MAX_AGE: Duration = Duration::from_secs(120);

#[derive(Default)]
pub(crate) struct HeldInput {
    queue: VecDeque<(ClientEndpointId, Instant, ClientMessage)>,
}

/// Whether a message is input the person typed or pasted into a pane.
pub(crate) fn is_input(message: &ClientMessage) -> bool {
    matches!(
        message,
        ClientMessage::ClientShellPaneInput { .. } | ClientMessage::ClientShellPopupInput { .. }
    )
}

impl HeldInput {
    pub(crate) fn hold(
        &mut self,
        endpoint_id: ClientEndpointId,
        now: Instant,
        message: ClientMessage,
    ) {
        self.queue.push_back((endpoint_id, now, message));
        while self.queue.len() > MAX_HELD {
            self.queue.pop_front();
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Whether input for this machine is waiting, so newer input queues behind it.
    pub(crate) fn holds_for(&self, endpoint_id: &ClientEndpointId) -> bool {
        self.queue.iter().any(|(id, _, _)| id == endpoint_id)
    }

    /// Takes this machine's input in the order it was typed, leaving out any
    /// older than [`MAX_AGE`].
    pub(crate) fn take(
        &mut self,
        endpoint_id: &ClientEndpointId,
        now: Instant,
    ) -> Vec<ClientMessage> {
        let mut taken = Vec::new();
        self.queue.retain(|(id, at, message)| {
            if id != endpoint_id {
                return now.saturating_duration_since(*at) < MAX_AGE;
            }
            if now.saturating_duration_since(*at) < MAX_AGE {
                taken.push(message.clone());
            }
            false
        });
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str) -> ClientMessage {
        ClientMessage::ClientShellPaneInput {
            pane_id: "w1:p1".into(),
            events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
                text.into(),
            )],
        }
    }

    #[test]
    fn held_input_goes_out_in_order_for_its_machine_only() {
        let now = Instant::now();
        let mut held = HeldInput::default();
        let other = ClientEndpointId::Local;
        held.hold(ClientEndpointId::Local, now, input("a"));
        held.hold(ClientEndpointId::Local, now, input("b"));
        assert!(held.holds_for(&other));
        assert_eq!(
            held.take(&ClientEndpointId::Local, now),
            vec![input("a"), input("b")]
        );
        assert!(held.is_empty());
    }

    #[test]
    fn stale_and_overflowing_input_is_dropped() {
        let now = Instant::now();
        let mut held = HeldInput::default();
        held.hold(ClientEndpointId::Local, now, input("old"));
        held.hold(ClientEndpointId::Local, now + MAX_AGE, input("new"));
        assert_eq!(
            held.take(&ClientEndpointId::Local, now + MAX_AGE),
            vec![input("new")]
        );
        for n in 0..MAX_HELD + 3 {
            held.hold(ClientEndpointId::Local, now, input(&n.to_string()));
        }
        let taken = held.take(&ClientEndpointId::Local, now);
        assert_eq!(taken.len(), MAX_HELD);
        assert_eq!(taken[0], input("3"));
    }

    #[test]
    fn only_typed_and_pasted_input_is_held() {
        assert!(is_input(&input("x")));
        assert!(!is_input(&ClientMessage::ClientShellFocus {
            focused: true
        }));
    }
}
