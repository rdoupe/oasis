use std::time::{Duration, Instant};

pub(super) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
pub(super) const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(10);
/// A server answers a probe at once, so one left unanswered this long means the
/// connection has stalled: the person is told before the transport gives up.
pub(super) const STALL_AFTER: Duration = Duration::from_secs(2);
/// Input this long after anything arrived probes at once instead of waiting for
/// the quiet interval, so a stall shows soon after the person types.
const INPUT_PROBE_AFTER: Duration = Duration::from_secs(1);
// Match the remote setup allowance without extending established transport health checks.
const INITIAL_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HealthAction {
    None,
    Ping,
    Expired,
}

pub(super) struct EndpointHealth {
    connected_at: Instant,
    last_received: Option<Instant>,
    ping_sent_at: Option<Instant>,
    ready: bool,
    probe_due: bool,
}

impl EndpointHealth {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            connected_at: now,
            last_received: None,
            ping_sent_at: None,
            ready: false,
            probe_due: false,
        }
    }

    pub(super) fn received(&mut self, now: Instant) {
        self.last_received = Some(now);
        self.ping_sent_at = None;
    }

    pub(super) fn ready(&mut self) {
        self.ready = true;
    }

    pub(super) fn resume(&mut self) {
        self.ping_sent_at = None;
        self.probe_due = true;
    }

    pub(super) fn action(&self, now: Instant) -> HealthAction {
        let initial_snapshot_expired = !self.ready
            && now.saturating_duration_since(self.connected_at) >= INITIAL_SNAPSHOT_TIMEOUT;
        // Welcome precedes state-loop registration, which can postpone the first pong.
        let probe_expired = (self.ready || self.last_received.is_some())
            && self
                .ping_sent_at
                .is_some_and(|sent_at| now.saturating_duration_since(sent_at) >= HEARTBEAT_TIMEOUT);
        if initial_snapshot_expired || probe_expired {
            HealthAction::Expired
        } else if self.probe_due
            || (self.ping_sent_at.is_none()
                && now.saturating_duration_since(self.last_received.unwrap_or(self.connected_at))
                    >= HEARTBEAT_INTERVAL)
        {
            HealthAction::Ping
        } else {
            HealthAction::None
        }
    }

    /// Whether a probe has gone unanswered for [`STALL_AFTER`].
    pub(super) fn stalled(&self, now: Instant) -> bool {
        (self.ready || self.last_received.is_some())
            && self
                .ping_sent_at
                .is_some_and(|sent_at| now.saturating_duration_since(sent_at) >= STALL_AFTER)
    }

    /// Input went out: probe now if the connection has been quiet, so a stall
    /// under the person's typing shows within seconds.
    pub(super) fn input_sent(&mut self, now: Instant) {
        if self.ping_sent_at.is_none()
            && now.saturating_duration_since(self.last_received.unwrap_or(self.connected_at))
                >= INPUT_PROBE_AFTER
        {
            self.probe_due = true;
        }
    }

    pub(super) fn ping_sent(&mut self, now: Instant) {
        self.probe_due = false;
        self.ping_sent_at = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_connection_is_probed_then_expires_without_a_reply() {
        let now = Instant::now();
        let mut health = EndpointHealth::new(now);
        health.ready();
        assert_eq!(health.action(now), HealthAction::None);
        assert_eq!(health.action(now + HEARTBEAT_INTERVAL), HealthAction::Ping);
        health.ping_sent(now + HEARTBEAT_INTERVAL);
        assert_eq!(
            health.action(now + HEARTBEAT_INTERVAL + HEARTBEAT_TIMEOUT),
            HealthAction::Expired
        );
    }

    #[test]
    fn pause_reprobes_without_extending_initial_readiness_budget() {
        let now = Instant::now();
        let mut health = EndpointHealth::new(now);
        health.received(now);
        health.ping_sent(now);
        health.resume();
        assert_eq!(
            health.action(now + Duration::from_secs(59)),
            HealthAction::Ping
        );
        health.ping_sent(now + Duration::from_secs(59));
        health.resume();
        assert_eq!(
            health.action(now + Duration::from_secs(60)),
            HealthAction::Expired
        );
    }

    #[test]
    fn an_unanswered_probe_reads_as_stalled_until_anything_arrives() {
        let now = Instant::now();
        let mut health = EndpointHealth::new(now);
        health.ready();
        health.ping_sent(now);
        assert!(!health.stalled(now + STALL_AFTER - Duration::from_millis(1)));
        assert!(health.stalled(now + STALL_AFTER));
        health.received(now + STALL_AFTER);
        assert!(!health.stalled(now + STALL_AFTER));
    }

    #[test]
    fn input_on_a_quiet_connection_probes_at_once() {
        let now = Instant::now();
        let mut health = EndpointHealth::new(now);
        health.ready();
        health.received(now);
        health.input_sent(now + Duration::from_millis(500));
        assert_eq!(
            health.action(now + Duration::from_millis(500)),
            HealthAction::None,
            "just heard from"
        );
        health.input_sent(now + INPUT_PROBE_AFTER);
        assert_eq!(health.action(now + INPUT_PROBE_AFTER), HealthAction::Ping);
    }

    #[test]
    fn any_incoming_message_satisfies_an_outstanding_probe() {
        let now = Instant::now();
        let mut health = EndpointHealth::new(now);
        health.ready();
        health.ping_sent(now);
        health.received(now + HEARTBEAT_TIMEOUT - Duration::from_millis(1));
        assert_eq!(health.action(now + HEARTBEAT_TIMEOUT), HealthAction::None);
    }

    #[test]
    fn delayed_registration_can_reply_after_the_normal_probe_deadline() {
        let now = Instant::now();
        let mut health = EndpointHealth::new(now);
        health.ping_sent(now + HEARTBEAT_INTERVAL);
        let registered = now + Duration::from_secs(45);
        assert_eq!(health.action(registered), HealthAction::None);
        health.received(registered);
        health.ready();
        assert_eq!(health.action(registered), HealthAction::None);
    }

    #[test]
    fn responding_startup_gets_time_for_its_snapshot_but_cannot_wait_forever() {
        let now = Instant::now();
        let mut health = EndpointHealth::new(now);
        for seconds in (5..60).step_by(5) {
            let reply = now + Duration::from_secs(seconds);
            health.received(reply);
            assert_eq!(health.action(reply), HealthAction::None);
        }
        assert_eq!(
            health.action(now + Duration::from_secs(60)),
            HealthAction::Expired
        );
    }

    #[test]
    fn startup_silence_is_bounded_and_confirmed_transport_keeps_normal_probe_timeout() {
        let now = Instant::now();
        let mut health = EndpointHealth::new(now);
        health.ping_sent(now + HEARTBEAT_INTERVAL);
        assert_eq!(
            health.action(now + Duration::from_secs(60)),
            HealthAction::Expired
        );

        let mut health = EndpointHealth::new(now);
        health.received(now + Duration::from_secs(6));
        health.ping_sent(now + Duration::from_secs(11));
        assert_eq!(
            health.action(now + Duration::from_secs(20)),
            HealthAction::None
        );
        assert_eq!(
            health.action(now + Duration::from_secs(21)),
            HealthAction::Expired
        );
    }
}
