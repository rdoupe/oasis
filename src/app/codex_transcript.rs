//! Keeps each Codex pane's running-subagent count current from its session
//! transcripts (see `codex_subagents`), for the thread its title names.
//!
//! The transcripts are read off the app thread when the agent's status
//! changes. A subagent can finish without waking its parent, so while any
//! run, they are checked again every few seconds; a check of a transcript
//! that has not grown reads nothing. Panes without running subagents cost
//! nothing.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tracing::warn;

use super::App;
use crate::detect::codex_subagents::{
    title_thread_prefix, transcript_for_thread_prefix, CodexSubagentWatch,
};
use crate::events::AppEvent;
use crate::layout::PaneId;

/// Codex writes the turn's last lines just after its screen goes idle.
const SETTLE: Duration = Duration::from_millis(400);
/// How often running subagents are checked again.
const RECHECK: Duration = Duration::from_secs(5);

impl App {
    /// The agent's status changed: read what changed in the thread the pane
    /// shows. An agent that exited took its subagents with it.
    pub(crate) fn codex_agent_status_changed(&mut self, pane_id: PaneId, released: bool) {
        if released {
            let Some(terminal) = self.codex_terminal_mut(pane_id) else {
                return;
            };
            if terminal.codex_subagents.take().is_some() {
                terminal.revision = terminal.revision.saturating_add(1);
                self.emit_codex_pane_updated(pane_id);
            }
            return;
        }
        self.bind_codex_thread_from_title(pane_id);
        self.scan_codex_subagents(pane_id, SETTLE);
    }

    /// The pane's title changed: a new thread (`/new`, a resume) moves the
    /// watch. A spinner turning changes nothing.
    pub(crate) fn codex_title_changed(&mut self, pane_id: PaneId) {
        if self.bind_codex_thread_from_title(pane_id) {
            self.scan_codex_subagents(pane_id, SETTLE);
        }
    }

    /// Watches the thread whose id the pane's title ends in, when exactly one
    /// transcript has that id. The title comes from the Codex inside the pane;
    /// hook reports can come from another pane's environment, so they are not
    /// used. Returns whether the watch changed.
    fn bind_codex_thread_from_title(&mut self, pane_id: PaneId) -> bool {
        let Some(terminal) = self.codex_terminal_mut(pane_id) else {
            return false;
        };
        if terminal.effective_agent_label() != Some("codex") {
            return false;
        }
        let Some(prefix) = terminal
            .terminal_title
            .as_deref()
            .and_then(title_thread_prefix)
        else {
            return false;
        };
        let needle = format!("-{prefix}");
        let watching = terminal.codex_subagents.as_ref().is_some_and(|watch| {
            watch
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".jsonl"))
                .and_then(|name| name.get(name.len().checked_sub(37)?..))
                .is_some_and(|id| id.starts_with(&needle))
        });
        if watching {
            return false;
        }
        // A new thread's transcript appears with its first prompt.
        let Some(path) = codex_sessions_dir()
            .and_then(|sessions| transcript_for_thread_prefix(&sessions, prefix))
        else {
            return false;
        };
        terminal.codex_subagents = Some(CodexSubagentWatch::new(path));
        terminal.revision = terminal.revision.saturating_add(1);
        true
    }

    fn scan_codex_subagents(&mut self, pane_id: PaneId, delay: Duration) {
        let Some(watch) = self
            .codex_terminal_mut(pane_id)
            .and_then(|terminal| terminal.codex_subagents.as_mut())
        else {
            return;
        };
        if watch.scanning {
            watch.rescan = true;
            return;
        }
        let Some(sessions) = codex_sessions_dir() else {
            return;
        };
        watch.scanning = true;
        let payload = (watch.clone(), sessions, self.event_tx.clone());
        let spawned = crate::thread_spawn::spawn_named_with(
            "oasis-codex-transcript",
            payload,
            move |(mut watch, sessions, event_tx)| {
                std::thread::sleep(delay);
                watch.advance(&sessions, SystemTime::now());
                let _ = event_tx.blocking_send(AppEvent::CodexSubagentsScanned { pane_id, watch });
            },
        );
        if let Err((_, err)) = spawned {
            warn!(err = %err, "failed to spawn codex transcript read");
            if let Some(watch) = self
                .codex_terminal_mut(pane_id)
                .and_then(|terminal| terminal.codex_subagents.as_mut())
            {
                watch.scanning = false;
            }
        }
    }

    pub(crate) fn handle_codex_subagents_scanned(
        &mut self,
        pane_id: PaneId,
        scanned: CodexSubagentWatch,
    ) {
        let Some(terminal) = self.codex_terminal_mut(pane_id) else {
            return;
        };
        // The agent exited or a new session replaced the watch meanwhile.
        let Some(watch) = terminal
            .codex_subagents
            .as_mut()
            .filter(|watch| watch.path == scanned.path)
        else {
            return;
        };
        let rescan = std::mem::take(&mut watch.rescan);
        let changed = watch.running != scanned.running;
        *watch = CodexSubagentWatch {
            scanning: false,
            rescan: false,
            ..scanned
        };
        let running = watch.running.unwrap_or(0) > 0;
        if changed {
            terminal.revision = terminal.revision.saturating_add(1);
            self.emit_codex_pane_updated(pane_id);
        }
        if rescan {
            self.scan_codex_subagents(pane_id, SETTLE);
        } else if running {
            self.scan_codex_subagents(pane_id, RECHECK);
        }
    }

    fn codex_terminal_mut(
        &mut self,
        pane_id: PaneId,
    ) -> Option<&mut crate::terminal::TerminalState> {
        let terminal_id = self
            .state
            .workspaces
            .iter()
            .find_map(|workspace| workspace.pane_state(pane_id))?
            .attached_terminal_id
            .clone();
        self.state.terminals.get_mut(&terminal_id)
    }

    fn emit_codex_pane_updated(&mut self, pane_id: PaneId) {
        if let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|workspace| workspace.pane_state(pane_id).is_some())
        {
            self.emit_pane_updated(ws_idx, pane_id);
        }
    }
}

fn codex_sessions_dir() -> Option<PathBuf> {
    crate::integration::env::codex_dir()
        .ok()
        .map(|home| home.join("sessions"))
}
