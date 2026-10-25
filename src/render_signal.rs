use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::layout::PaneId;

#[derive(Debug, Default)]
pub(crate) struct RenderRequest {
    pub(crate) generic: bool,
    pub(crate) pty_sources: HashSet<PaneId>,
    pub(crate) terminal_title_sources: HashSet<PaneId>,
}

/// Coalesces render requests while retaining enough origin information for the
/// headless server to discard PTY-only updates hidden from every client.
#[derive(Debug, Default)]
pub(crate) struct RenderSignal {
    pending: AtomicBool,
    state: Mutex<RenderSignalState>,
}

#[derive(Debug, Default)]
struct RenderSignalState {
    request: RenderRequest,
    immediate_pty_sources: HashSet<PaneId>,
    /// Set once a consumer publishes which sources are visible. From then on
    /// a source outside that set never wakes anyone: its damage waits for the
    /// next wake, which discards it unless the source became visible.
    visibility_tracked: bool,
}

impl RenderSignal {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Acquire)
    }

    pub(crate) fn request_generic(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.request.generic = true;
        self.pending.store(true, Ordering::Release);
    }

    /// Returns true when the signal becomes pending or visible PTY work joins it.
    pub(crate) fn request_pty(&self, pane_id: PaneId) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source_added = state.request.pty_sources.insert(pane_id);
        let immediate = state.immediate_pty_sources.contains(&pane_id);
        if state.visibility_tracked && !immediate {
            return false;
        }
        let wake_for_source = source_added && immediate;
        let became_pending = !self.pending.swap(true, Ordering::AcqRel);
        became_pending || wake_for_source
    }

    /// Whether `pane_id` is known to be hidden from every render target, so
    /// presentation-only follow-ups for it can be skipped.
    pub(crate) fn is_pty_source_hidden(&self, pane_id: PaneId) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.visibility_tracked && !state.immediate_pty_sources.contains(&pane_id)
    }

    pub(crate) fn set_immediate_pty_sources(&self, sources: HashSet<PaneId>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.immediate_pty_sources = sources;
        state.visibility_tracked = true;
    }

    pub(crate) fn has_immediate_work(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.request.generic
            || !state.request.terminal_title_sources.is_empty()
            || state
                .request
                .pty_sources
                .iter()
                .any(|pane_id| state.immediate_pty_sources.contains(pane_id))
    }

    pub(crate) fn has_pending_visible_pty_source(&self, eligible: impl Fn(PaneId) -> bool) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .request
            .pty_sources
            .iter()
            .any(|pane_id| state.immediate_pty_sources.contains(pane_id) && eligible(*pane_id))
    }

    /// Coalesces terminal-title changes separately from ordinary PTY damage so
    /// consumers can update metadata without inspecting every pane.
    pub(crate) fn request_terminal_title(&self, pane_id: PaneId) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source_added = state.request.terminal_title_sources.insert(pane_id);
        let became_pending = !self.pending.swap(true, Ordering::AcqRel);
        became_pending || source_added
    }

    pub(crate) fn pending_terminal_title_sources(&self) -> HashSet<PaneId> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .request
            .terminal_title_sources
            .clone()
    }

    pub(crate) fn take(&self) -> RenderRequest {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.pending.store(false, Ordering::Release);
        std::mem::take(&mut state.request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_pty_sources_until_taken() {
        let signal = RenderSignal::new();
        let first = PaneId::from_raw(10);
        let second = PaneId::from_raw(20);

        assert!(signal.request_pty(first));
        assert!(!signal.request_pty(first));
        assert!(!signal.request_pty(second));

        let request = signal.take();
        assert!(!request.generic);
        assert_eq!(request.pty_sources, HashSet::from([first, second]));
        assert!(request.terminal_title_sources.is_empty());
        assert!(!signal.is_pending());
    }

    #[test]
    fn untracked_pty_sources_coalesce_to_one_wake() {
        let signal = RenderSignal::new();

        let wakes = (1..=50)
            .filter(|pane_id| signal.request_pty(PaneId::from_raw(*pane_id)))
            .count();

        assert_eq!(wakes, 1);
        assert!(!signal.is_pty_source_hidden(PaneId::from_raw(1)));
    }

    #[test]
    fn hidden_pty_sources_never_wake_once_visibility_is_tracked() {
        let signal = RenderSignal::new();
        signal.set_immediate_pty_sources(HashSet::from([PaneId::from_raw(100)]));

        let wakes = (1..=50)
            .filter(|pane_id| signal.request_pty(PaneId::from_raw(*pane_id)))
            .count();

        assert_eq!(wakes, 0);
        assert!(!signal.is_pending());
        assert!(signal.is_pty_source_hidden(PaneId::from_raw(1)));
        assert!(!signal.is_pty_source_hidden(PaneId::from_raw(100)));
        // The damage is kept for the next wake, which can see a new visibility.
        assert_eq!(signal.take().pty_sources.len(), 50);
    }

    #[test]
    fn immediate_pty_source_wakes_pending_hidden_work() {
        let signal = RenderSignal::new();
        let hidden = PaneId::from_raw(10);
        let visible = PaneId::from_raw(20);
        signal.set_immediate_pty_sources(HashSet::from([visible]));

        assert!(!signal.request_pty(hidden));
        assert!(!signal.request_pty(PaneId::from_raw(30)));
        assert!(signal.request_pty(visible));
        assert!(!signal.request_pty(visible));
        assert_eq!(
            signal.take().pty_sources,
            HashSet::from([hidden, visible, PaneId::from_raw(30)])
        );
    }

    #[test]
    fn hidden_source_that_becomes_visible_wakes_on_its_next_output() {
        let signal = RenderSignal::new();
        let pane_id = PaneId::from_raw(10);
        signal.set_immediate_pty_sources(HashSet::new());
        assert!(!signal.request_pty(pane_id));

        signal.set_immediate_pty_sources(HashSet::from([pane_id]));
        assert!(signal.request_pty(pane_id));
    }

    #[test]
    fn terminal_title_source_wakes_pending_pty_work() {
        let signal = RenderSignal::new();
        let pane_id = PaneId::from_raw(10);

        assert!(signal.request_pty(pane_id));
        assert!(signal.request_terminal_title(pane_id));
        assert!(!signal.request_terminal_title(pane_id));
    }

    #[test]
    fn coalesces_terminal_title_sources_without_making_them_pty_damage() {
        let signal = RenderSignal::new();
        let pane_id = PaneId::from_raw(10);

        assert!(signal.request_terminal_title(pane_id));
        assert!(!signal.request_terminal_title(pane_id));
        assert_eq!(
            signal.pending_terminal_title_sources(),
            HashSet::from([pane_id])
        );

        let request = signal.take();
        assert!(request.pty_sources.is_empty());
        assert_eq!(request.terminal_title_sources, HashSet::from([pane_id]));
    }

    #[test]
    fn keeps_generic_and_pty_requests_distinct() {
        let signal = RenderSignal::new();
        let pane_id = PaneId::from_raw(10);

        signal.request_generic();
        assert!(!signal.request_pty(pane_id));

        let request = signal.take();
        assert!(request.generic);
        assert_eq!(request.pty_sources, HashSet::from([pane_id]));
    }
}
