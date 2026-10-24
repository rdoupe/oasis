//! Background activity for agent panes on servers that do not compute it.
//!
//! Oasis servers parse an agent's footer (shells, monitors, subagents) and send
//! the counts as reserved `oasis:` agent tokens. Herdr servers have no such
//! parser, so for their Claude panes the client reads the pane's detection text
//! through the server's JSON API and runs the same parser itself. Reads happen
//! only when the pane's agent data changes, at most one per pane every
//! [`REMOTE_ACTIVITY_MIN_INTERVAL`] and one in flight per endpoint, so an idle
//! client sends nothing.

use super::*;

/// The shortest gap between two reads of one pane; a change inside it is read
/// once the gap has passed.
pub(crate) const REMOTE_ACTIVITY_MIN_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(3);

const ACTIVITY_TOKEN_PREFIX: &str = "oasis:";

/// The agent data whose change may mean the footer changed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct AgentSignature {
    status: crate::api::schema::AgentStatus,
    state_change_seq: u64,
    title: Option<String>,
    terminal_title: Option<String>,
}

impl AgentSignature {
    fn of(agent: &crate::protocol::ClientShellAgent) -> Self {
        Self {
            status: agent.agent_status,
            state_change_seq: agent.state_change_seq,
            title: agent.title.clone(),
            terminal_title: agent.terminal_title.clone(),
        }
    }
}

#[derive(Clone, Debug)]
struct PaneActivity {
    signature: AgentSignature,
    dirty: bool,
    last_request: Option<std::time::Instant>,
    tokens: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct EndpointRemoteActivity {
    /// The server does not send activity itself, and reads have not failed.
    enabled: bool,
    panes: HashMap<String, PaneActivity>,
    in_flight: Option<String>,
}

impl EndpointRemoteActivity {
    fn reset(&mut self, enabled: bool) {
        *self = Self {
            enabled,
            ..Self::default()
        };
    }

    /// Tracks the snapshot's Claude panes, marks changed ones for a read, and
    /// attaches the activity read so far.
    fn observe(&mut self, snapshot: &mut ClientShellSnapshot) {
        if !self.enabled {
            return;
        }
        let mut seen = HashSet::new();
        for agent in &snapshot.agents {
            if agent.agent.as_deref() != Some("claude") {
                continue;
            }
            seen.insert(agent.pane_id.clone());
            let signature = AgentSignature::of(agent);
            match self.panes.get_mut(&agent.pane_id) {
                Some(pane) if pane.signature == signature => {}
                Some(pane) => {
                    pane.signature = signature;
                    pane.dirty = true;
                }
                None => {
                    self.panes.insert(
                        agent.pane_id.clone(),
                        PaneActivity {
                            signature,
                            dirty: true,
                            last_request: None,
                            tokens: Vec::new(),
                        },
                    );
                }
            }
        }
        self.panes.retain(|pane_id, _| seen.contains(pane_id));
        if self
            .in_flight
            .as_ref()
            .is_some_and(|pane_id| !self.panes.contains_key(pane_id))
        {
            self.in_flight = None;
        }
        self.apply(snapshot);
    }

    fn apply(&self, snapshot: &mut ClientShellSnapshot) {
        for agent in &mut snapshot.agents {
            let Some(pane) = self.panes.get(&agent.pane_id) else {
                continue;
            };
            agent
                .tokens
                .retain(|(key, _)| !key.starts_with(ACTIVITY_TOKEN_PREFIX));
            agent.tokens.extend(pane.tokens.iter().cloned());
            agent.tokens.sort_by(|left, right| left.0.cmp(&right.0));
        }
    }

    fn due(&self, pane: &PaneActivity, now: std::time::Instant) -> Option<std::time::Instant> {
        pane.dirty.then(|| {
            pane.last_request
                .map_or(now, |last| last + REMOTE_ACTIVITY_MIN_INTERVAL)
        })
    }

    fn next_due(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        if !self.enabled || self.in_flight.is_some() {
            return None;
        }
        self.panes
            .values()
            .filter_map(|pane| self.due(pane, now))
            .min()
    }

    fn take_due(&mut self, now: std::time::Instant) -> Option<String> {
        if !self.enabled || self.in_flight.is_some() {
            return None;
        }
        let pane_id = self
            .panes
            .iter()
            .filter_map(|(pane_id, pane)| Some((self.due(pane, now)?, pane_id)))
            .filter(|(due, _)| *due <= now)
            .min()
            .map(|(_, pane_id)| pane_id.clone())?;
        let pane = self.panes.get_mut(&pane_id)?;
        pane.dirty = false;
        pane.last_request = Some(now);
        self.in_flight = Some(pane_id.clone());
        Some(pane_id)
    }

    /// Records a read. Returns whether the pane's tokens changed.
    fn receive(&mut self, pane_id: &str, text: Option<&str>) -> bool {
        if self.in_flight.as_deref() == Some(pane_id) {
            self.in_flight = None;
        }
        let Some(text) = text else {
            // The server cannot serve the read: stay quiet and stop asking.
            let had_tokens = self.panes.values().any(|pane| !pane.tokens.is_empty());
            self.reset(false);
            return had_tokens;
        };
        let Some(pane) = self.panes.get_mut(pane_id) else {
            return false;
        };
        // No footer on screen (a dialog): the last counts stand.
        let Some(activity) =
            crate::detect::activity::background_activity(Some(crate::detect::Agent::Claude), text)
        else {
            return false;
        };
        let tokens = crate::api::schema::BackgroundActivityInfo::from_activity(&activity)
            .map(|info| info.sidebar_tokens())
            .unwrap_or_default();
        if pane.tokens == tokens {
            return false;
        }
        pane.tokens = tokens;
        true
    }
}

impl ClientShellState {
    /// Whether the endpoint's server computes background activity itself. Local
    /// is always an oasis server; other endpoints read it client-side if not.
    pub(crate) fn set_endpoint_background_activity_supported(
        &mut self,
        endpoint_id: &ClientEndpointId,
        supported: bool,
    ) {
        if let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        {
            endpoint
                .remote_activity
                .reset(!supported && !endpoint_id.is_local());
        }
    }

    pub(super) fn observe_remote_activity(
        &mut self,
        index: usize,
        snapshot: &mut ClientShellSnapshot,
    ) {
        if let Some(endpoint) = self.endpoints.get_mut(index) {
            endpoint.remote_activity.observe(snapshot);
        }
    }

    /// When the next pane read is due, for the client loop's timer.
    pub(crate) fn remote_activity_due(
        &self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        self.endpoints
            .iter()
            .filter_map(|endpoint| endpoint.remote_activity.next_due(now))
            .min()
    }

    /// Pane reads that are due now, at most one per endpoint.
    pub(crate) fn take_remote_activity_reads(
        &mut self,
        now: std::time::Instant,
    ) -> Vec<(ClientEndpointId, String)> {
        self.endpoints
            .iter_mut()
            .filter_map(|endpoint| {
                endpoint
                    .remote_activity
                    .take_due(now)
                    .map(|pane_id| (endpoint.endpoint_id.clone(), pane_id))
            })
            .collect()
    }

    /// Applies a pane's detection text (`None` when the server could not serve
    /// it). Returns whether the sidebar needs a repaint.
    pub(crate) fn receive_remote_agent_screen(
        &mut self,
        endpoint_id: &ClientEndpointId,
        pane_id: &str,
        text: Option<&str>,
    ) -> bool {
        let Some(endpoint) = self
            .endpoints
            .iter_mut()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        else {
            return false;
        };
        if !endpoint.remote_activity.receive(pane_id, text) {
            return false;
        }
        let activity = endpoint.remote_activity.clone();
        if let Some(snapshot) = endpoint.snapshot.as_deref_mut() {
            clear_activity_tokens(snapshot);
            activity.apply(snapshot);
        }
        if endpoint_id == &self.active_endpoint_id {
            if let Some(snapshot) = self.snapshot.as_deref_mut() {
                clear_activity_tokens(snapshot);
                activity.apply(snapshot);
            }
        }
        true
    }
}

/// Activity a herdr server never sends is only ever ours, so it can be cleared
/// before reapplying (including after reads were disabled).
fn clear_activity_tokens(snapshot: &mut ClientShellSnapshot) {
    for agent in &mut snapshot.agents {
        agent
            .tokens
            .retain(|(key, _)| !key.starts_with(ACTIVITY_TOKEN_PREFIX));
    }
}
