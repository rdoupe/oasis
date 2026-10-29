use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Instant;

use super::health::{EndpointHealth, HealthAction};
use super::input_sequence::InputSequence;
use super::ClientEndpointId;
use crate::protocol::ClientMessage;

pub(crate) trait EndpointTransport: Send {
    fn send(&mut self, message: &ClientMessage) -> io::Result<()>;

    fn disconnect(&mut self) {}

    fn flush(&mut self, _deadline: Instant) -> io::Result<()> {
        Ok(())
    }

    fn take_error(&mut self) -> Option<io::Error> {
        None
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct EndpointNegotiation {
    methods: HashSet<String>,
    capabilities: HashSet<String>,
}

impl EndpointNegotiation {
    pub(crate) fn new(methods: Vec<String>, capabilities: Vec<String>) -> Self {
        Self {
            methods: methods.into_iter().collect(),
            capabilities: capabilities.into_iter().collect(),
        }
    }

    pub(crate) fn capabilities(&self) -> Vec<String> {
        let mut capabilities = self.capabilities.iter().cloned().collect::<Vec<_>>();
        capabilities.sort();
        capabilities
    }

    pub(crate) fn methods(&self) -> Vec<String> {
        self.methods.iter().cloned().collect()
    }

    pub(crate) fn supports_method(&self, method: &str) -> bool {
        self.methods.contains(method)
    }

    pub(crate) fn supports_capability(&self, capability: &str) -> bool {
        self.capabilities.contains(capability)
    }

    pub(crate) fn supports_surface_interest(&self) -> bool {
        self.supports_capability(crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY)
            && self.supports_capability(
                crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY,
            )
            && self.supports_method("client_shell.surface.set")
    }

    pub(crate) fn supports_health_check(&self) -> bool {
        self.supports_capability(crate::protocol::endpoint::HEALTH_CHECK_CAPABILITY)
    }

    pub(crate) fn supports_input_sequence(&self) -> bool {
        self.supports_capability(crate::protocol::endpoint::INPUT_SEQUENCE_CAPABILITY)
    }
}

pub(crate) struct EndpointConnection {
    transport: Box<dyn EndpointTransport>,
    pub(crate) generation: u64,
    pub(crate) surface_active: bool,
    pub(crate) negotiation: EndpointNegotiation,
    health: Option<EndpointHealth>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EndpointTransportFailure {
    pub(crate) endpoint_id: ClientEndpointId,
    pub(crate) generation: u64,
    pub(crate) kind: io::ErrorKind,
    pub(crate) message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndpointSendOutcome {
    Sent,
    NotSent,
}

pub(crate) struct EndpointRegistry {
    active: ClientEndpointId,
    input_enabled: bool,
    connections: HashMap<ClientEndpointId, EndpointConnection>,
    failures: Vec<EndpointTransportFailure>,
    last_health_tick: Instant,
    /// Numbered input per machine; it outlives connections so a reconnect can
    /// send what the last one didn't deliver.
    input_sequences: HashMap<ClientEndpointId, InputSequence>,
}

impl EndpointRegistry {
    pub(crate) fn empty() -> Self {
        Self {
            active: ClientEndpointId::Local,
            input_enabled: false,
            connections: HashMap::new(),
            failures: Vec::new(),
            last_health_tick: Instant::now(),
            input_sequences: HashMap::new(),
        }
    }

    pub(crate) fn new(
        local: impl EndpointTransport + 'static,
        generation: u64,
        negotiation: EndpointNegotiation,
    ) -> Self {
        let mut registry = Self::empty();
        registry.input_enabled = true;
        registry.insert(
            ClientEndpointId::Local,
            local,
            generation,
            negotiation,
            true,
        );
        registry
    }

    pub(crate) fn active_id(&self) -> &ClientEndpointId {
        &self.active
    }

    pub(crate) fn active_surface_available(&self) -> bool {
        self.input_enabled
            && self
                .connections
                .get(&self.active)
                .is_some_and(|connection| connection.surface_active)
    }

    pub(crate) fn select_unavailable_local(&mut self) {
        self.active = ClientEndpointId::Local;
        self.freeze_input();
    }

    pub(crate) fn freeze_input(&mut self) {
        self.input_enabled = false;
    }

    pub(crate) fn unfreeze_input(&mut self) {
        self.input_enabled = true;
    }

    pub(crate) fn connection(&self, endpoint_id: &ClientEndpointId) -> Option<&EndpointConnection> {
        self.connections.get(endpoint_id)
    }

    pub(crate) fn insert(
        &mut self,
        endpoint_id: ClientEndpointId,
        transport: impl EndpointTransport + 'static,
        generation: u64,
        negotiation: EndpointNegotiation,
        surface_active: bool,
    ) {
        let health = (!endpoint_id.is_local() && negotiation.supports_health_check())
            .then(|| EndpointHealth::new(Instant::now()));
        let numbered = negotiation.supports_input_sequence();
        if let Some(mut previous) = self.connections.insert(
            endpoint_id.clone(),
            EndpointConnection {
                transport: Box::new(transport),
                generation,
                surface_active,
                negotiation,
                health,
            },
        ) {
            previous.transport.disconnect();
        }
        if numbered {
            // Input waits until the server says how far this machine's input got.
            let resume = self
                .input_sequences
                .entry(endpoint_id.clone())
                .or_insert_with(InputSequence::new)
                .begin_resume(generation);
            self.send_to(&endpoint_id, &resume);
        } else if let Some(mut sequence) = self.input_sequences.remove(&endpoint_id) {
            let left = sequence.abandon();
            if left.unknown > 0 {
                tracing::warn!(
                    endpoint = %endpoint_id.storage_key(),
                    count = left.unknown,
                    "input sent before the connection dropped may not have arrived"
                );
            }
            for (_, message) in left.send {
                if self.send_to(&endpoint_id, &message) != EndpointSendOutcome::Sent {
                    break;
                }
            }
        }
    }

    /// The server applied this machine's input up to the acknowledged number.
    pub(crate) fn input_acked(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        ack: &crate::protocol::endpoint::EndpointInputAck,
    ) {
        if !self.accepts(endpoint_id, generation) {
            return;
        }
        if let Some(sequence) = self.input_sequences.get_mut(endpoint_id) {
            sequence.acked(ack);
        }
    }

    /// The server said how far this machine's input got: sends the rest, in
    /// order, ahead of anything typed since. Returns how many inputs went out
    /// before the connection dropped and can't be accounted for, because the
    /// server restarted; those are not sent again.
    pub(crate) fn input_resumed(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        answer: &crate::protocol::endpoint::EndpointInputResumed,
    ) -> usize {
        if !self.accepts(endpoint_id, generation) {
            return 0;
        }
        let Some(resumed) = self
            .input_sequences
            .get_mut(endpoint_id)
            .and_then(|sequence| sequence.resumed(generation, answer))
        else {
            return 0;
        };
        if !resumed.send.is_empty() {
            tracing::info!(
                endpoint = %endpoint_id.storage_key(),
                count = resumed.send.len(),
                "sending input the server had not applied"
            );
        }
        for (seq, message) in &resumed.send {
            if !self.send_numbered(endpoint_id, *seq, message) {
                break;
            }
        }
        resumed.unknown
    }

    /// Sends one numbered input: its number, then the input.
    fn send_numbered(
        &mut self,
        endpoint_id: &ClientEndpointId,
        seq: u64,
        message: &ClientMessage,
    ) -> bool {
        let Some(marker) = self
            .input_sequences
            .get(endpoint_id)
            .map(|sequence| sequence.marker(seq))
        else {
            return false;
        };
        let sent = self.send_plain(endpoint_id, &marker) == EndpointSendOutcome::Sent
            && self.send_plain(endpoint_id, message) == EndpointSendOutcome::Sent;
        if sent {
            if let Some(sequence) = self.input_sequences.get_mut(endpoint_id) {
                sequence.mark_sent(seq);
            }
        }
        sent
    }

    pub(crate) fn accepts(&self, endpoint_id: &ClientEndpointId, generation: u64) -> bool {
        self.connections
            .get(endpoint_id)
            .is_some_and(|connection| connection.generation == generation)
    }

    pub(crate) fn received(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        now: Instant,
    ) {
        if let Some(health) = self
            .connections
            .get_mut(endpoint_id)
            .filter(|connection| connection.generation == generation)
            .and_then(|connection| connection.health.as_mut())
        {
            health.received(now);
        }
    }

    /// Connections whose last probe has gone unanswered; see `health::STALL_AFTER`.
    pub(crate) fn unresponsive(&self, now: Instant) -> std::collections::HashSet<ClientEndpointId> {
        self.connections
            .iter()
            .filter(|(_, connection)| {
                connection
                    .health
                    .as_ref()
                    .is_some_and(|health| health.stalled(now))
            })
            .map(|(endpoint_id, _)| endpoint_id.clone())
            .collect()
    }

    /// Records input sent to an endpoint, probing it if it has been quiet.
    pub(crate) fn input_sent(&mut self, endpoint_id: &ClientEndpointId, now: Instant) {
        if let Some(health) = self
            .connections
            .get_mut(endpoint_id)
            .and_then(|connection| connection.health.as_mut())
        {
            health.input_sent(now);
        }
    }

    pub(crate) fn mark_ready(&mut self, endpoint_id: &ClientEndpointId, generation: u64) {
        if let Some(health) = self
            .connections
            .get_mut(endpoint_id)
            .filter(|connection| connection.generation == generation)
            .and_then(|connection| connection.health.as_mut())
        {
            health.ready();
        }
    }

    /// Whether any connection has a health check that needs ticking.
    pub(crate) fn health_tracked(&self) -> bool {
        self.connections
            .values()
            .any(|connection| connection.health.is_some())
    }

    pub(crate) fn resume_health(&mut self, now: Instant) {
        self.last_health_tick = now;
        for connection in self.connections.values_mut() {
            if let Some(health) = &mut connection.health {
                health.resume();
            }
        }
    }

    pub(crate) fn tick_health(&mut self, now: Instant) {
        // The client could not consume queued replies during a local scheduling pause.
        if now.saturating_duration_since(self.last_health_tick) > super::health::HEARTBEAT_TIMEOUT {
            self.resume_health(now);
        }
        self.last_health_tick = now;
        let actions = self
            .connections
            .iter()
            .filter_map(|(endpoint_id, connection)| {
                connection
                    .health
                    .as_ref()
                    .map(|health| (endpoint_id.clone(), health.action(now)))
            })
            .filter(|(_, action)| *action != HealthAction::None)
            .collect::<Vec<_>>();
        for (endpoint_id, action) in actions {
            match action {
                HealthAction::None => {}
                HealthAction::Ping => {
                    let ping = ClientMessage::EndpointControl {
                        kind: crate::protocol::endpoint::HEALTH_PING_KIND.into(),
                        data: String::new(),
                    };
                    if self.send_to(&endpoint_id, &ping) == EndpointSendOutcome::Sent {
                        if let Some(health) = self
                            .connections
                            .get_mut(&endpoint_id)
                            .and_then(|connection| connection.health.as_mut())
                        {
                            health.ping_sent(now);
                        }
                    }
                }
                HealthAction::Expired => self.record_failure(
                    endpoint_id,
                    io::Error::new(io::ErrorKind::TimedOut, "endpoint health check timed out"),
                ),
            }
        }
    }

    pub(crate) fn set_active(&mut self, endpoint_id: &ClientEndpointId) -> bool {
        if !self
            .connections
            .get(endpoint_id)
            .is_some_and(|connection| connection.surface_active)
        {
            return false;
        }
        self.active = endpoint_id.clone();
        true
    }

    pub(crate) fn set_surface_active(
        &mut self,
        endpoint_id: &ClientEndpointId,
        active: bool,
    ) -> bool {
        let Some(connection) = self.connections.get_mut(endpoint_id) else {
            return false;
        };
        let changed = connection.surface_active != active;
        connection.surface_active = active;
        changed
    }

    pub(crate) fn send(&mut self, message: &ClientMessage) -> EndpointSendOutcome {
        let endpoint_id = self.active.clone();
        self.send_to(&endpoint_id, message)
    }

    /// Sends a message to a machine. Typed and pasted input to a server that
    /// numbers it is kept until acknowledged; while a new connection waits to
    /// hear how far earlier input got, it queues and counts as sent.
    pub(crate) fn send_to(
        &mut self,
        endpoint_id: &ClientEndpointId,
        message: &ClientMessage,
    ) -> EndpointSendOutcome {
        // Between a dropped connection and its replacement, input for a machine
        // that numbers it waits for the replacement as well.
        let numbered = crate::client::held_input::is_input(message)
            && match self.connections.get(endpoint_id) {
                Some(connection) => connection.negotiation.supports_input_sequence(),
                None => self.input_sequences.contains_key(endpoint_id),
            };
        if numbered {
            let connected = self.connections.contains_key(endpoint_id);
            let sequence = self
                .input_sequences
                .entry(endpoint_id.clone())
                .or_insert_with(InputSequence::new);
            let seq = sequence.number(message.clone(), Instant::now());
            if sequence.awaiting() || !connected {
                return EndpointSendOutcome::Sent;
            }
            return if self.send_numbered(endpoint_id, seq, message) {
                EndpointSendOutcome::Sent
            } else {
                EndpointSendOutcome::NotSent
            };
        }
        self.send_plain(endpoint_id, message)
    }

    fn send_plain(
        &mut self,
        endpoint_id: &ClientEndpointId,
        message: &ClientMessage,
    ) -> EndpointSendOutcome {
        let result = self
            .connections
            .get_mut(endpoint_id)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "endpoint is unavailable"))
            .and_then(|connection| connection.transport.send(message));
        match result {
            Ok(()) => EndpointSendOutcome::Sent,
            Err(error) => {
                self.record_failure(endpoint_id.clone(), error);
                EndpointSendOutcome::NotSent
            }
        }
    }

    pub(crate) fn disconnect(&mut self, endpoint_id: &ClientEndpointId) {
        self.failures
            .retain(|failure| &failure.endpoint_id != endpoint_id);
        if let Some(mut connection) = self.connections.remove(endpoint_id) {
            connection.transport.disconnect();
        }
    }

    pub(crate) fn fail(&mut self, endpoint_id: &ClientEndpointId, error: io::Error) {
        self.record_failure(endpoint_id.clone(), error);
    }

    pub(crate) fn take_failures(&mut self) -> Vec<EndpointTransportFailure> {
        let errors = self
            .connections
            .iter_mut()
            .filter_map(|(id, connection)| {
                connection
                    .transport
                    .take_error()
                    .map(|error| (id.clone(), error))
            })
            .collect::<Vec<_>>();
        for (endpoint_id, error) in errors {
            self.record_failure(endpoint_id, error);
        }
        std::mem::take(&mut self.failures)
    }

    fn record_failure(&mut self, endpoint_id: ClientEndpointId, error: io::Error) {
        let Some(generation) = self
            .connections
            .get(&endpoint_id)
            .map(|connection| connection.generation)
        else {
            return;
        };
        let failure = EndpointTransportFailure {
            endpoint_id: endpoint_id.clone(),
            generation,
            kind: error.kind(),
            message: error.to_string(),
        };
        if let Some(mut connection) = self.connections.remove(&endpoint_id) {
            connection.transport.disconnect();
        }
        if let Some(existing) = self
            .failures
            .iter_mut()
            .find(|existing| existing.endpoint_id == endpoint_id)
        {
            *existing = failure;
        } else {
            self.failures.push(failure);
        }
    }
}

impl Drop for EndpointRegistry {
    fn drop(&mut self) {
        let deadline = Instant::now() + std::time::Duration::from_millis(250);
        for connection in self.connections.values_mut() {
            let _ = connection.transport.send(&ClientMessage::Detach);
        }
        for connection in self.connections.values_mut() {
            let _ = connection.transport.flush(deadline);
            connection.transport.disconnect();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    struct FakeTransport {
        sent: Arc<Mutex<Vec<ClientMessage>>>,
        error: Option<io::ErrorKind>,
    }

    impl EndpointTransport for FakeTransport {
        fn send(&mut self, message: &ClientMessage) -> io::Result<()> {
            if let Some(kind) = self.error {
                return Err(io::Error::new(kind, "fake transport failure"));
            }
            self.sent.lock().unwrap().push(message.clone());
            Ok(())
        }
    }

    fn negotiation() -> EndpointNegotiation {
        EndpointNegotiation::new(
            vec!["client_shell.surface.set".into()],
            vec![
                crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into(),
                crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
                crate::protocol::endpoint::HEALTH_CHECK_CAPABILITY.into(),
            ],
        )
    }

    fn profile() -> crate::client::endpoint::ProfileId {
        crate::client::endpoint::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap()
    }

    #[test]
    fn endpoint_failures_do_not_remove_other_connections() {
        let local_sent = Arc::new(Mutex::new(Vec::new()));
        let mut registry = EndpointRegistry::new(
            FakeTransport {
                sent: local_sent.clone(),
                error: None,
            },
            1,
            negotiation(),
        );
        let ssh_id = ClientEndpointId::Ssh(profile());
        registry.insert(
            ssh_id.clone(),
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: Some(io::ErrorKind::BrokenPipe),
            },
            2,
            negotiation(),
            true,
        );
        assert!(registry.set_active(&ssh_id));

        assert_eq!(
            registry.send(&ClientMessage::ClientShellFocus { focused: true }),
            EndpointSendOutcome::NotSent
        );
        assert!(registry.connection(&ssh_id).is_none());
        assert!(registry.connection(&ClientEndpointId::Local).is_some());
        assert_eq!(registry.take_failures()[0].endpoint_id, ssh_id);

        assert!(registry.set_active(&ClientEndpointId::Local));
        assert_eq!(
            registry.send(&ClientMessage::ClientShellFocus { focused: true }),
            EndpointSendOutcome::Sent
        );
        assert_eq!(local_sent.lock().unwrap().len(), 1);
    }

    #[test]
    fn reconnecting_active_identity_does_not_count_as_an_active_surface() {
        let mut registry = EndpointRegistry::new(
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            1,
            negotiation(),
        );
        let ssh_id = ClientEndpointId::Ssh(profile());
        registry.insert(
            ssh_id.clone(),
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            2,
            negotiation(),
            true,
        );
        assert!(registry.set_active(&ssh_id));
        assert!(registry.active_surface_available());
        registry.disconnect(&ssh_id);
        registry.insert(
            ssh_id,
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            3,
            negotiation(),
            false,
        );
        assert!(!registry.active_surface_available());
    }

    #[test]
    fn recovered_local_uses_transport_failure_not_remote_health_probes() {
        let mut registry = EndpointRegistry::empty();
        let sent = Arc::new(Mutex::new(Vec::new()));
        registry.insert(
            ClientEndpointId::Local,
            FakeTransport {
                sent: sent.clone(),
                error: None,
            },
            2,
            negotiation(),
            false,
        );
        registry.tick_health(Instant::now() + std::time::Duration::from_secs(300));
        assert!(registry.connection(&ClientEndpointId::Local).is_some());
        assert!(sent.lock().unwrap().is_empty());
        assert!(registry.take_failures().is_empty());
    }

    #[test]
    fn negotiated_remote_health_probe_expires_the_connection() {
        let mut registry = EndpointRegistry::new(
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            1,
            negotiation(),
        );
        let ssh_id = ClientEndpointId::Ssh(profile());
        let sent = Arc::new(Mutex::new(Vec::new()));
        registry.insert(
            ssh_id.clone(),
            FakeTransport {
                sent: sent.clone(),
                error: None,
            },
            2,
            negotiation(),
            false,
        );
        let now = Instant::now();
        registry.mark_ready(&ssh_id, 2);
        registry.tick_health(now + super::super::health::HEARTBEAT_INTERVAL);
        assert!(matches!(
            sent.lock().unwrap().as_slice(),
            [ClientMessage::EndpointControl { kind, .. }]
                if kind == crate::protocol::endpoint::HEALTH_PING_KIND
        ));

        registry.tick_health(
            now + super::super::health::HEARTBEAT_INTERVAL
                + super::super::health::HEARTBEAT_TIMEOUT,
        );
        assert!(registry.connection(&ssh_id).is_none());
        assert_eq!(registry.take_failures()[0].kind, io::ErrorKind::TimedOut);
    }

    #[test]
    fn paused_client_probes_before_expiring_a_live_connection() {
        let mut registry = EndpointRegistry::empty();
        let id = ClientEndpointId::Ssh(profile());
        let sent = Arc::new(Mutex::new(Vec::new()));
        registry.insert(
            id.clone(),
            FakeTransport {
                sent: sent.clone(),
                error: None,
            },
            2,
            negotiation(),
            true,
        );
        let now = Instant::now();
        registry.mark_ready(&id, 2);
        registry.tick_health(now + std::time::Duration::from_secs(5));
        // The client could not run while the old probe's reply was waiting in its queue.
        let resumed = now + std::time::Duration::from_secs(90);
        registry.tick_health(resumed);
        assert!(
            registry.connection(&id).is_some(),
            "local scheduling pause is not peer failure"
        );
        assert_eq!(sent.lock().unwrap().len(), 2, "send a fresh probe");
        registry.received(&id, 2, resumed);
        registry.tick_health(resumed + std::time::Duration::from_secs(1));
        assert!(registry.take_failures().is_empty());
    }

    #[test]
    fn a_ready_endpoint_can_stay_connected_after_the_initial_deadline() {
        let mut registry = EndpointRegistry::new(
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            1,
            negotiation(),
        );
        let ssh_id = ClientEndpointId::Ssh(profile());
        registry.insert(
            ssh_id.clone(),
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            2,
            negotiation(),
            false,
        );
        let now = Instant::now();
        registry.mark_ready(&ssh_id, 2);
        registry.received(&ssh_id, 2, now + std::time::Duration::from_secs(80));
        registry.tick_health(now + std::time::Duration::from_secs(80));
        assert!(registry.connection(&ssh_id).is_some());
        assert!(!registry.connection(&ssh_id).unwrap().surface_active);
    }

    fn numbering() -> EndpointNegotiation {
        EndpointNegotiation::new(
            Vec::new(),
            vec![crate::protocol::endpoint::INPUT_SEQUENCE_CAPABILITY.into()],
        )
    }

    fn typed(text: &str) -> ClientMessage {
        ClientMessage::ClientShellPaneInput {
            pane_id: "w1:p1".into(),
            events: vec![crate::protocol::ClientPaneInputEvent::TextCommit(
                text.into(),
            )],
        }
    }

    /// A server's read loop, as far as numbered input goes: it applies each
    /// number once, through the same ledger the real server uses.
    struct FakeServer {
        ledger: crate::server::input_ledger::InputLedger,
        instance: String,
        applied: Vec<ClientMessage>,
        acks: Vec<crate::protocol::endpoint::EndpointInputAck>,
    }

    impl FakeServer {
        fn new(instance: &str) -> Self {
            Self {
                ledger: Default::default(),
                instance: instance.into(),
                applied: Vec::new(),
                acks: Vec::new(),
            }
        }

        /// Reads what a connection delivered; returns the resume answer, if asked.
        fn read(
            &mut self,
            messages: &[ClientMessage],
        ) -> Option<crate::protocol::endpoint::EndpointInputResumed> {
            use crate::protocol::endpoint as wire;
            let mut pending: Option<wire::EndpointInputSequence> = None;
            let mut answer = None;
            for message in messages {
                match message {
                    ClientMessage::EndpointControl { kind, data }
                        if kind == wire::INPUT_SEQUENCE_KIND =>
                    {
                        pending = Some(serde_json::from_str(data).unwrap());
                    }
                    ClientMessage::EndpointControl { kind, data }
                        if kind == wire::INPUT_RESUME_KIND =>
                    {
                        let resume: wire::EndpointInputResume = serde_json::from_str(data).unwrap();
                        answer = Some(wire::EndpointInputResumed {
                            last_applied: self.ledger.last_applied(&resume.token),
                            token: resume.token,
                            instance: self.instance.clone(),
                        });
                    }
                    message if crate::client::held_input::is_input(message) => {
                        let Some(sequence) = pending.take() else {
                            self.applied.push(message.clone());
                            continue;
                        };
                        if self.ledger.admits(&sequence.token, sequence.seq) {
                            self.applied.push(message.clone());
                            self.ledger
                                .record(&sequence.token, sequence.seq, Instant::now());
                        }
                        self.acks.push(wire::EndpointInputAck {
                            token: sequence.token,
                            seq: sequence.seq,
                            instance: self.instance.clone(),
                        });
                    }
                    _ => {}
                }
            }
            answer
        }
    }

    fn connect(
        registry: &mut EndpointRegistry,
        endpoint: &ClientEndpointId,
        generation: u64,
        negotiation: EndpointNegotiation,
    ) -> Arc<Mutex<Vec<ClientMessage>>> {
        let sent = Arc::new(Mutex::new(Vec::new()));
        registry.insert(
            endpoint.clone(),
            FakeTransport {
                sent: sent.clone(),
                error: None,
            },
            generation,
            negotiation,
            true,
        );
        sent
    }

    fn drain(sent: &Arc<Mutex<Vec<ClientMessage>>>) -> Vec<ClientMessage> {
        sent.lock().unwrap().drain(..).collect()
    }

    #[test]
    fn input_reaches_the_server_exactly_once_across_a_dropped_connection() {
        let endpoint = ClientEndpointId::Ssh(profile());
        let mut server = FakeServer::new("srv");
        let mut registry = EndpointRegistry::empty();

        let first = connect(&mut registry, &endpoint, 1, numbering());
        let answer = server.read(&drain(&first)).unwrap();
        assert_eq!(registry.input_resumed(&endpoint, 1, &answer), 0);

        for text in ["a", "b", "c"] {
            registry.send_to(&endpoint, &typed(text));
        }
        // The server reads a and b; c is stuck in the stalled connection, and
        // only a's acknowledgement gets back.
        let in_flight = drain(&first);
        assert_eq!(in_flight.len(), 6, "each input follows its number");
        server.read(&in_flight[..4]);
        let ack = server.acks[0].clone();
        registry.input_acked(&endpoint, 1, &ack);

        // The connection drops; d is typed before the next one is up.
        registry.fail(
            &endpoint,
            io::Error::new(io::ErrorKind::TimedOut, "stalled"),
        );
        registry.send_to(&endpoint, &typed("d"));

        let second = connect(&mut registry, &endpoint, 2, numbering());
        registry.send_to(&endpoint, &typed("e"));
        let resume = drain(&second);
        assert_eq!(resume.len(), 1, "input waits for the server's answer");
        let answer = server.read(&resume).unwrap();
        assert_eq!(answer.last_applied, Some(2));
        assert_eq!(registry.input_resumed(&endpoint, 2, &answer), 0);
        server.read(&drain(&second));
        // The stalled connection finally delivers its copy of c.
        server.read(&in_flight[4..]);

        assert_eq!(
            server.applied,
            ["a", "b", "c", "d", "e"].map(typed).to_vec(),
            "every input once, in the order typed"
        );
    }

    #[test]
    fn a_restarted_server_gets_no_input_it_may_already_have() {
        let endpoint = ClientEndpointId::Ssh(profile());
        let mut old = FakeServer::new("old");
        let mut registry = EndpointRegistry::empty();
        let first = connect(&mut registry, &endpoint, 1, numbering());
        let answer = old.read(&drain(&first)).unwrap();
        registry.input_resumed(&endpoint, 1, &answer);
        registry.send_to(&endpoint, &typed("a"));
        drain(&first);
        registry.fail(&endpoint, io::Error::new(io::ErrorKind::TimedOut, "gone"));

        let mut restarted = FakeServer::new("new");
        let second = connect(&mut registry, &endpoint, 2, numbering());
        let answer = restarted.read(&drain(&second)).unwrap();
        assert_eq!(answer.last_applied, None);
        assert_eq!(
            registry.input_resumed(&endpoint, 2, &answer),
            1,
            "a's fate is unknown, so it is reported, not typed again"
        );
        assert!(second.lock().unwrap().is_empty());
        registry.send_to(&endpoint, &typed("b"));
        restarted.read(&drain(&second));
        assert_eq!(restarted.applied, vec![typed("b")]);
    }

    #[test]
    fn a_server_without_numbering_gets_plain_input_as_before() {
        let endpoint = ClientEndpointId::Ssh(profile());
        let mut registry = EndpointRegistry::empty();
        let sent = connect(&mut registry, &endpoint, 1, negotiation());
        registry.send_to(&endpoint, &typed("a"));
        assert_eq!(*sent.lock().unwrap(), vec![typed("a")]);
    }

    #[test]
    fn answers_for_a_replaced_connection_are_ignored() {
        let endpoint = ClientEndpointId::Ssh(profile());
        let mut server = FakeServer::new("srv");
        let mut registry = EndpointRegistry::empty();
        let first = connect(&mut registry, &endpoint, 1, numbering());
        let stale = server.read(&drain(&first)).unwrap();
        let second = connect(&mut registry, &endpoint, 2, numbering());
        assert_eq!(registry.input_resumed(&endpoint, 1, &stale), 0);
        registry.send_to(&endpoint, &typed("a"));
        assert_eq!(
            second.lock().unwrap().len(),
            1,
            "still waiting on the new connection's answer"
        );
    }

    #[test]
    fn negotiated_surface_interest_requires_capability_and_method() {
        assert!(negotiation().supports_surface_interest());
        assert!(negotiation().supports_health_check());
        assert!(
            !EndpointNegotiation::new(vec!["client_shell.surface.set".into()], Vec::new())
                .supports_surface_interest()
        );
        assert!(!EndpointNegotiation::new(
            Vec::new(),
            vec![crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into()]
        )
        .supports_surface_interest());
        assert!(!EndpointNegotiation::new(
            vec!["client_shell.surface.set".into()],
            vec![crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into()]
        )
        .supports_surface_interest());
    }

    #[test]
    fn dropping_registry_detaches_every_connected_endpoint() {
        let local_sent = Arc::new(Mutex::new(Vec::new()));
        let remote_sent = Arc::new(Mutex::new(Vec::new()));
        let mut registry = EndpointRegistry::new(
            FakeTransport {
                sent: local_sent.clone(),
                error: None,
            },
            1,
            negotiation(),
        );
        registry.insert(
            ClientEndpointId::Ssh(profile()),
            FakeTransport {
                sent: remote_sent.clone(),
                error: None,
            },
            2,
            negotiation(),
            false,
        );

        drop(registry);

        assert!(matches!(
            local_sent.lock().unwrap().as_slice(),
            [ClientMessage::Detach]
        ));
        assert!(matches!(
            remote_sent.lock().unwrap().as_slice(),
            [ClientMessage::Detach]
        ));
    }

    #[test]
    fn stale_generations_are_rejected() {
        let registry = EndpointRegistry::new(
            FakeTransport {
                sent: Arc::new(Mutex::new(Vec::new())),
                error: None,
            },
            7,
            negotiation(),
        );
        assert!(registry.accepts(&ClientEndpointId::Local, 7));
        assert!(!registry.accepts(&ClientEndpointId::Local, 6));
    }
}
