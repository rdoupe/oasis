//! Keeps each Claude pane's loop state current from its session transcript.
//!
//! The transcript is read off the app thread, and only when the hook reports
//! a session or the agent's status changes: a loop arms, fires and ends at
//! turn boundaries, so nothing is polled and idle panes cost nothing.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::warn;

use super::App;
use crate::detect::claude_loop::{ClaudeLoopTracker, ClaudeTranscriptWatch, LoopStatus};
use crate::events::AppEvent;
use crate::layout::PaneId;

/// Claude writes the turn's last lines just after its screen goes idle.
const SETTLE: Duration = Duration::from_millis(400);
/// One read's cap, so a burst of large tool output is parsed in pieces.
const MAX_READ: u64 = 8 * 1024 * 1024;

impl App {
    /// Starts watching the transcript Claude's session hook reported. The
    /// same path again (a compaction) keeps what was read.
    pub(crate) fn watch_claude_transcript(&mut self, pane_id: PaneId, path: String) {
        let path = PathBuf::from(path);
        if !path.is_absolute() || path.extension().is_none_or(|ext| ext != "jsonl") {
            return;
        }
        let Some(terminal) = self.pane_terminal_state_mut(pane_id) else {
            return;
        };
        if terminal
            .claude_transcript
            .as_ref()
            .is_some_and(|watch| watch.path == path)
        {
            return;
        }
        terminal.claude_transcript = Some(ClaudeTranscriptWatch::new(path));
        self.scan_claude_transcript(pane_id);
    }

    /// The agent's status changed: read what the transcript gained. An agent
    /// that exited took its wakeups and jobs with it.
    pub(crate) fn claude_agent_status_changed(&mut self, pane_id: PaneId, released: bool) {
        if released {
            let Some(terminal) = self.pane_terminal_state_mut(pane_id) else {
                return;
            };
            if terminal
                .claude_transcript
                .take()
                .and_then(|watch| watch.loop_label)
                .is_some()
            {
                terminal.revision = terminal.revision.saturating_add(1);
                self.emit_pane_updated_for(pane_id);
            }
            return;
        }
        self.scan_claude_transcript(pane_id);
    }

    fn scan_claude_transcript(&mut self, pane_id: PaneId) {
        let Some(watch) = self
            .pane_terminal_state_mut(pane_id)
            .and_then(|terminal| terminal.claude_transcript.as_mut())
        else {
            return;
        };
        if watch.scanning {
            watch.rescan = true;
            return;
        }
        watch.scanning = true;
        let payload = (
            watch.path.clone(),
            watch.tracker.clone(),
            self.event_tx.clone(),
        );
        let spawned = crate::thread_spawn::spawn_named_with(
            "oasis-claude-transcript",
            payload,
            move |(path, tracker, event_tx)| {
                std::thread::sleep(SETTLE);
                let tracker = advance(&path, tracker);
                let _ = event_tx.blocking_send(AppEvent::ClaudeTranscriptScanned {
                    pane_id,
                    path,
                    tracker,
                });
            },
        );
        if let Err((_, err)) = spawned {
            warn!(err = %err, "failed to spawn claude transcript read");
            if let Some(watch) = self
                .pane_terminal_state_mut(pane_id)
                .and_then(|terminal| terminal.claude_transcript.as_mut())
            {
                watch.scanning = false;
            }
        }
    }

    pub(crate) fn handle_claude_transcript_scanned(
        &mut self,
        pane_id: PaneId,
        path: PathBuf,
        tracker: ClaudeLoopTracker,
    ) {
        let Some(terminal) = self.pane_terminal_state_mut(pane_id) else {
            return;
        };
        // A newer session replaced the watch while this read ran.
        let Some(watch) = terminal
            .claude_transcript
            .as_mut()
            .filter(|watch| watch.path == path)
        else {
            return;
        };
        let label = tracker.status().map(loop_label);
        watch.tracker = tracker;
        watch.scanning = false;
        let rescan = std::mem::take(&mut watch.rescan);
        if watch.loop_label != label {
            watch.loop_label = label;
            terminal.revision = terminal.revision.saturating_add(1);
            self.emit_pane_updated_for(pane_id);
        }
        if rescan {
            self.scan_claude_transcript(pane_id);
        }
    }

    fn pane_terminal_state_mut(
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

    fn emit_pane_updated_for(&mut self, pane_id: PaneId) {
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

/// Reads what the transcript gained since the tracker's offset.
fn advance(path: &Path, mut tracker: ClaudeLoopTracker) -> ClaudeLoopTracker {
    let Ok(mut file) = std::fs::File::open(path) else {
        // A new session's transcript appears with its first message; read it
        // from the start then.
        tracker.offset.get_or_insert(0);
        return tracker;
    };
    let len = file.metadata().map_or(0, |metadata| metadata.len());
    let Some(offset) = tracker.offset else {
        // A resumed session: earlier wakeups died with the earlier process.
        tracker.offset = Some(len);
        return tracker;
    };
    if len < offset {
        return ClaudeLoopTracker::starting_at(len);
    }
    let mut offset = offset;
    while offset < len {
        let mut chunk = Vec::new();
        let read = file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| (&mut file).take(MAX_READ).read_to_end(&mut chunk));
        let Ok(read) = read else {
            break;
        };
        if read == 0 {
            break;
        }
        let consumed = tracker.feed(&chunk);
        if consumed == 0 {
            if (read as u64) < MAX_READ {
                break; // A line still being written.
            }
            // One line longer than a read holds nothing a loop writes.
            tracker.offset = Some(offset + read as u64);
        }
        offset = tracker.offset.unwrap_or(len);
    }
    tracker
}

fn loop_label(status: LoopStatus) -> String {
    match status {
        LoopStatus::NextWakeup(at) => local_clock(at).unwrap_or_default(),
        LoopStatus::Interval(interval) => interval,
        LoopStatus::Active => String::new(),
    }
}

/// `HH:MM` local time of a Unix timestamp.
fn local_clock(unix: i64) -> Option<String> {
    let local_now = crate::platform::local_datetime()?
        .assume_utc()
        .unix_timestamp();
    let utc_now = time::OffsetDateTime::now_utc().unix_timestamp();
    // The two clocks are read a moment apart; zones are whole quarter hours.
    let offset = ((local_now - utc_now) as f64 / 900.0).round() as i64 * 900;
    let at = time::OffsetDateTime::from_unix_timestamp(unix + offset).ok()?;
    Some(format!("{:02}:{:02}", at.hour(), at.minute()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_transcript(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oasis-claude-transcript-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("session.jsonl")
    }

    const LOOP_COMMAND: &str = r#"{"type":"user","timestamp":"2026-10-24T04:55:26.138Z","message":{"content":"<command-name>/loop</command-name>"}}"#;

    #[test]
    fn a_resumed_transcript_is_read_from_its_end() {
        let path = temp_transcript("resume");
        std::fs::write(&path, format!("{LOOP_COMMAND}\n")).unwrap();
        let tracker = advance(&path, ClaudeLoopTracker::default());
        assert_eq!(tracker.status(), None, "the old process's loop is gone");
        std::fs::write(&path, format!("{LOOP_COMMAND}\n{LOOP_COMMAND}\n")).unwrap();
        let tracker = advance(&path, tracker);
        assert_eq!(tracker.status(), Some(LoopStatus::Active));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_new_session_is_read_from_its_start_once_written() {
        let path = temp_transcript("new");
        let tracker = advance(&path, ClaudeLoopTracker::default());
        assert_eq!(tracker.offset, Some(0));
        std::fs::write(&path, format!("{LOOP_COMMAND}\n")).unwrap();
        let tracker = advance(&path, tracker);
        assert_eq!(tracker.status(), Some(LoopStatus::Active));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn local_clock_formats_hours_and_minutes() {
        let clock = local_clock(1_761_282_000).unwrap();
        assert_eq!(clock.len(), 5);
        assert_eq!(&clock[2..3], ":");
    }
}
