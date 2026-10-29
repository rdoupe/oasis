//! Numbered input for one machine, kept until the server says it applied it.
//!
//! Each pane or popup input message goes out right after a sequence control
//! that numbers it. The server applies each number once and acknowledges it.
//! When a connection drops, input sent on it may or may not have arrived; on
//! the next connection the client asks how far its input got and sends only the
//! rest, so nothing is typed twice or lost. Input typed while that answer is
//! awaited waits behind the replay.

use std::collections::VecDeque;
use std::hash::{BuildHasher, Hasher};
use std::time::{Duration, Instant};

use crate::protocol::endpoint::{
    EndpointInputAck, EndpointInputResume, EndpointInputResumed, EndpointInputSequence,
};
use crate::protocol::ClientMessage;

/// More unacknowledged input than this and the oldest is forgotten, as held
/// input is: a stuck key must not grow without bound.
const MAX_UNACKED: usize = 512;
/// Unacknowledged input older than this is forgotten rather than typed long
/// after the person moved on.
const MAX_AGE: Duration = Duration::from_secs(120);

struct Numbered {
    seq: u64,
    at: Instant,
    message: ClientMessage,
    /// Whether it went out on a connection, so it may have been applied.
    sent: bool,
}

pub(crate) struct InputSequence {
    token: String,
    next_seq: u64,
    unacked: VecDeque<Numbered>,
    /// The connection generation whose resume answer is awaited; input waits until it comes.
    awaiting: Option<u64>,
    /// The server process that acknowledged this token's input.
    instance: Option<String>,
}

/// What a resume answer leaves to do.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Resumed {
    /// Input to send now, in order, each with its number.
    pub(crate) send: Vec<(u64, ClientMessage)>,
    /// Input sent before the connection dropped whose fate is unknown: the
    /// server restarted and forgot what it applied. It is not sent again.
    pub(crate) unknown: usize,
}

impl InputSequence {
    pub(crate) fn new() -> Self {
        Self::with_token(random_token())
    }

    fn with_token(token: String) -> Self {
        Self {
            token,
            next_seq: 1,
            unacked: VecDeque::new(),
            awaiting: None,
            instance: None,
        }
    }

    /// Numbers `message` and keeps it until it is acknowledged.
    pub(crate) fn number(&mut self, message: ClientMessage, now: Instant) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.unacked.push_back(Numbered {
            seq,
            at: now,
            message,
            sent: false,
        });
        while self.unacked.len() > MAX_UNACKED
            || self
                .unacked
                .front()
                .is_some_and(|oldest| now.saturating_duration_since(oldest.at) >= MAX_AGE)
        {
            if let Some(dropped) = self.unacked.pop_front() {
                tracing::warn!(seq = dropped.seq, "forgetting unacknowledged input");
            }
        }
        seq
    }

    /// Whether input waits for a resume answer before going out.
    pub(crate) fn awaiting(&self) -> bool {
        self.awaiting.is_some()
    }

    /// The control that numbers the input message sent right after it.
    pub(crate) fn marker(&self, seq: u64) -> ClientMessage {
        control(
            crate::protocol::endpoint::INPUT_SEQUENCE_KIND,
            &EndpointInputSequence {
                token: self.token.clone(),
                seq,
            },
        )
    }

    pub(crate) fn mark_sent(&mut self, seq: u64) {
        if let Some(numbered) = self.unacked.iter_mut().find(|numbered| numbered.seq == seq) {
            numbered.sent = true;
        }
    }

    /// A new connection: input waits until the server says how far it got.
    pub(crate) fn begin_resume(&mut self, generation: u64) -> ClientMessage {
        self.awaiting = Some(generation);
        control(
            crate::protocol::endpoint::INPUT_RESUME_KIND,
            &EndpointInputResume {
                token: self.token.clone(),
            },
        )
    }

    /// The connection was replaced by one without numbered input: what went out
    /// may or may not have arrived, and what didn't is sent as plain input.
    pub(crate) fn abandon(&mut self) -> Resumed {
        self.awaiting = None;
        let mut resumed = Resumed::default();
        for numbered in self.unacked.drain(..) {
            if numbered.sent {
                resumed.unknown += 1;
            } else {
                resumed.send.push((numbered.seq, numbered.message));
            }
        }
        resumed
    }

    pub(crate) fn acked(&mut self, ack: &EndpointInputAck) {
        if ack.token != self.token {
            return;
        }
        self.unacked.retain(|numbered| numbered.seq > ack.seq);
        self.instance = Some(ack.instance.clone());
    }

    /// The server's answer for connection `generation`; `None` when it isn't the
    /// one awaited.
    pub(crate) fn resumed(
        &mut self,
        generation: u64,
        answer: &EndpointInputResumed,
    ) -> Option<Resumed> {
        if self.awaiting != Some(generation) || answer.token != self.token {
            return None;
        }
        self.awaiting = None;
        let mut resumed = Resumed::default();
        match answer.last_applied {
            Some(last) => self.unacked.retain(|numbered| numbered.seq > last),
            // A server that never acknowledged this token can't have forgotten it,
            // unless it is a different process from the one that did.
            None if self
                .instance
                .as_ref()
                .is_some_and(|instance| *instance != answer.instance) =>
            {
                self.unacked.retain(|numbered| {
                    if numbered.sent {
                        resumed.unknown += 1;
                    }
                    !numbered.sent
                });
            }
            None => {}
        }
        self.instance = Some(answer.instance.clone());
        resumed.send = self
            .unacked
            .iter()
            .map(|numbered| (numbered.seq, numbered.message.clone()))
            .collect();
        Some(resumed)
    }
}

fn control(kind: &str, value: &impl serde::Serialize) -> ClientMessage {
    let (kind, data) = crate::protocol::endpoint::json_control(kind, value)
        .unwrap_or_else(|| (kind.to_owned(), String::new()));
    ClientMessage::EndpointControl { kind, data }
}

/// 128 random bits as hex, from the standard library's randomly seeded hasher.
fn random_token() -> String {
    let half = || {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        hasher.finish()
    };
    format!("{:016x}{:016x}", half(), half())
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

    fn sent(sequence: &mut InputSequence, text: &str, now: Instant) -> u64 {
        let seq = sequence.number(input(text), now);
        sequence.mark_sent(seq);
        seq
    }

    fn ack(sequence: &InputSequence, seq: u64, instance: &str) -> EndpointInputAck {
        EndpointInputAck {
            token: sequence.token.clone(),
            seq,
            instance: instance.into(),
        }
    }

    fn answer(sequence: &InputSequence, last: Option<u64>, instance: &str) -> EndpointInputResumed {
        EndpointInputResumed {
            token: sequence.token.clone(),
            instance: instance.into(),
            last_applied: last,
        }
    }

    #[test]
    fn tokens_are_random_hex() {
        let (a, b) = (random_token(), random_token());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn only_input_the_server_did_not_apply_is_sent_again() {
        let now = Instant::now();
        let mut sequence = InputSequence::with_token("t".into());
        sent(&mut sequence, "a", now);
        sequence.acked(&ack(&sequence, 1, "srv"));
        sent(&mut sequence, "b", now);
        sent(&mut sequence, "c", now);
        sent(&mut sequence, "d", now);
        // The connection drops; the server had applied b but not its ack.
        sequence.begin_resume(2);
        assert!(sequence.awaiting());
        let typed_meanwhile = sequence.number(input("e"), now);
        assert_eq!(typed_meanwhile, 5);
        let resumed = sequence
            .resumed(2, &answer(&sequence, Some(2), "srv"))
            .unwrap();
        assert_eq!(
            resumed.send,
            vec![(3, input("c")), (4, input("d")), (5, input("e"))],
            "after what was applied, in order, the input typed meanwhile last"
        );
        assert_eq!(resumed.unknown, 0);
        assert!(!sequence.awaiting());
    }

    #[test]
    fn a_server_that_saw_nothing_gets_everything() {
        let now = Instant::now();
        let mut sequence = InputSequence::with_token("t".into());
        sequence.begin_resume(1);
        sequence
            .resumed(1, &answer(&sequence, None, "srv"))
            .unwrap();
        sent(&mut sequence, "a", now);
        sequence.begin_resume(2);
        let resumed = sequence
            .resumed(2, &answer(&sequence, None, "srv"))
            .unwrap();
        assert_eq!(resumed.send, vec![(1, input("a"))]);
        assert_eq!(resumed.unknown, 0);
    }

    #[test]
    fn a_restarted_server_leaves_sent_input_unknown_and_unsent() {
        let now = Instant::now();
        let mut sequence = InputSequence::with_token("t".into());
        sent(&mut sequence, "a", now);
        sequence.acked(&ack(&sequence, 1, "old"));
        sent(&mut sequence, "b", now);
        sequence.begin_resume(2);
        sequence.number(input("c"), now);
        let resumed = sequence
            .resumed(2, &answer(&sequence, None, "new"))
            .unwrap();
        assert_eq!(resumed.unknown, 1, "b may or may not have arrived");
        assert_eq!(resumed.send, vec![(3, input("c"))], "c never went out");
    }

    #[test]
    fn answers_for_another_connection_or_token_are_ignored() {
        let mut sequence = InputSequence::with_token("t".into());
        sequence.begin_resume(3);
        assert!(sequence
            .resumed(2, &answer(&sequence, None, "srv"))
            .is_none());
        let mut other = answer(&sequence, None, "srv");
        other.token = "u".into();
        assert!(sequence.resumed(3, &other).is_none());
        assert!(sequence.awaiting());
    }

    #[test]
    fn a_connection_without_numbering_sends_what_never_went_out() {
        let now = Instant::now();
        let mut sequence = InputSequence::with_token("t".into());
        sent(&mut sequence, "a", now);
        sequence.number(input("b"), now);
        let resumed = sequence.abandon();
        assert_eq!(resumed.unknown, 1);
        assert_eq!(resumed.send, vec![(2, input("b"))]);
    }

    #[test]
    fn unacknowledged_input_is_bounded() {
        let now = Instant::now();
        let mut sequence = InputSequence::with_token("t".into());
        sequence.number(input("old"), now);
        sequence.number(input("new"), now + MAX_AGE);
        assert_eq!(sequence.unacked.len(), 1);
        for n in 0..MAX_UNACKED + 3 {
            sequence.number(input(&n.to_string()), now + MAX_AGE);
        }
        assert_eq!(sequence.unacked.len(), MAX_UNACKED);
    }
}
