//! Keeps each Codex pane's running-subagent count current from its session
//! transcripts (see `codex_subagents`).
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
use crate::detect::codex_subagents::{transcript_for_thread, CodexSubagentWatch};
use crate::events::AppEvent;
use crate::layout::PaneId;

/// Codex writes the turn's last lines just after its screen goes idle.
const SETTLE: Duration = Duration::from_millis(400);
/// How often running subagents are checked again.
const RECHECK: Duration = Duration::from_secs(5);

impl App {
    /// The agent's status changed: start watching a Codex pane's thread by
    /// its session id, and read what changed. An agent that exited took its
    /// subagents with it.
    pub(crate) fn codex_agent_status_changed(&mut self, pane_id: PaneId, released: bool) {
        let Some(terminal) = self.codex_terminal_mut(pane_id) else {
            return;
        };
        if released {
            if terminal.codex_subagents.take().is_some() {
                terminal.revision = terminal.revision.saturating_add(1);
                self.emit_codex_pane_updated(pane_id);
            }
            return;
        }
        if terminal.effective_agent_label() != Some("codex") {
            return;
        }
        let Some(session) = super::creation::terminal_agent_session_info(terminal)
            .filter(|session| session.kind == crate::agent_resume::AgentSessionRefKind::Id)
        else {
            return;
        };
        // A new thread (`/new`, a resume) replaces the watch.
        let suffix = format!("-{}.jsonl", session.value);
        let watching = terminal.codex_subagents.as_ref().is_some_and(|watch| {
            watch
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&suffix))
        });
        if !watching {
            let Some(path) = codex_sessions_dir()
                .and_then(|sessions| transcript_for_thread(&sessions, &session.value))
            else {
                return;
            };
            terminal.codex_subagents = Some(CodexSubagentWatch::new(path));
        }
        self.scan_codex_subagents(pane_id, SETTLE);
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
