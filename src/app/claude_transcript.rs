//! Keeps each Claude pane's loop and goal state, and how many of its
//! subagents are unfinished, current from its session transcript.
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
            // What the transcript added (a loop, a goal, a subagent cap) goes.
            if terminal.claude_transcript.take().is_some() {
                terminal.revision = terminal.revision.saturating_add(1);
                self.emit_pane_updated_for(pane_id);
            }
            return;
        }
        self.find_claude_transcript(pane_id);
        self.scan_claude_transcript(pane_id);
    }

    /// A Claude pane known only by its session id, as one handed over from a
    /// Herdr server or restored, has no transcript watch until its session
    /// hook fires again; find the transcript by that id instead.
    fn find_claude_transcript(&mut self, pane_id: PaneId) {
        let Some(terminal) = self.pane_terminal_state_mut(pane_id) else {
            return;
        };
        if terminal.claude_transcript.is_some()
            || terminal.effective_agent_label() != Some("claude")
        {
            return;
        }
        let Some(session) = super::creation::terminal_agent_session_info(terminal)
            .filter(|session| session.kind == crate::agent_resume::AgentSessionRefKind::Id)
        else {
            return;
        };
        if let Some(path) = transcript_for_session(&session.value) {
            self.watch_claude_transcript(pane_id, path.to_string_lossy().into_owned());
        }
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
                let unfinished_subagents = unfinished_subagents(&path);
                let _ = event_tx.blocking_send(AppEvent::ClaudeTranscriptScanned {
                    pane_id,
                    path,
                    tracker,
                    unfinished_subagents,
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
        unfinished_subagents: Option<u16>,
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
        let goal = tracker.goal();
        watch.tracker = tracker;
        watch.scanning = false;
        let rescan = std::mem::take(&mut watch.rescan);
        if (&watch.loop_label, watch.goal, watch.unfinished_subagents)
            != (&label, goal, unfinished_subagents)
        {
            watch.loop_label = label;
            watch.goal = goal;
            watch.unfinished_subagents = unfinished_subagents;
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

/// `<claude config>/projects/<project>/<session id>.jsonl`, the project
/// folder unknown.
fn transcript_for_session(session_id: &str) -> Option<PathBuf> {
    if session_id.is_empty()
        || !session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return None;
    }
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))?;
    let file = format!("{session_id}.jsonl");
    std::fs::read_dir(config.join("projects"))
        .ok()?
        .flatten()
        .map(|project| project.path().join(&file))
        .find(|path| path.is_file())
}

/// Subagents whose records beside the transcript (`<session>/subagents/
/// *.meta.json`) are not marked finished. Claude keeps a finished one's row on
/// screen with its timer stopped; one that died with an earlier process never
/// finishes, but its row is gone, so callers cap the screen's row count.
fn unfinished_subagents(transcript: &Path) -> Option<u16> {
    let dir = transcript.with_extension("").join("subagents");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Some(0),
        Err(_) => return None,
    };
    let mut count = 0u16;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".meta.json"))
        {
            continue;
        }
        let finished = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|meta| meta.get("finished").and_then(serde_json::Value::as_bool))
            == Some(true);
        if !finished {
            count = count.saturating_add(1);
        }
    }
    Some(count)
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
    use crate::detect::claude_loop::TranscriptGoal;

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
    fn only_a_plain_session_id_names_a_transcript() {
        assert_eq!(transcript_for_session(""), None);
        assert_eq!(transcript_for_session("../../etc/passwd"), None);
    }

    #[test]
    fn counts_subagents_not_marked_finished() {
        let path = temp_transcript("subagents");
        assert_eq!(unfinished_subagents(&path), Some(0));
        let dir = path.with_extension("").join("subagents");
        std::fs::create_dir_all(&dir).unwrap();
        // Shapes captured from a stock Claude Code 2.1.310 session.
        std::fs::write(
            dir.join("agent-a1.meta.json"),
            r#"{"agentType":"general-purpose","requestShape":"background"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("agent-a2.meta.json"),
            r#"{"agentType":"general-purpose","requestShape":"background","finished":true}"#,
        )
        .unwrap();
        std::fs::write(dir.join("agent-a1.jsonl"), "{}\n").unwrap();
        assert_eq!(unfinished_subagents(&path), Some(1));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Anonymized excerpts of real sessions where a met goal never showed:
    /// each runs from the goal record before the met one to the end of the
    /// turn that met it.
    const MET_GOALS: [(&str, &str); 3] = [
        (
            "backup work",
            include_str!("../../tests/fixtures/claude-goal/met-after-backup-work.jsonl"),
        ),
        (
            "deploy",
            include_str!("../../tests/fixtures/claude-goal/met-after-deploy.jsonl"),
        ),
        (
            "firmware trace",
            include_str!("../../tests/fixtures/claude-goal/met-after-firmware-trace.jsonl"),
        ),
    ];
    /// The anonymized screen of the firmware trace session right after its
    /// goal was met.
    const SCREEN_GOAL_MET: &str =
        include_str!("../../tests/fixtures/claude-goal/screen-goal-met.txt");

    /// Milliseconds into the day of a transcript line's timestamp.
    fn line_millis(line: &str) -> Option<u64> {
        let at = line.find("\"timestamp\":\"")? + "\"timestamp\":\"".len();
        let clock = line.get(at + 11..at + 23)?; // HH:MM:SS.mmm
        let part = |range: std::ops::Range<usize>| clock.get(range)?.parse::<u64>().ok();
        Some(((part(0..2)? * 60 + part(3..5)?) * 60 + part(6..8)?) * 1000 + part(9..12)?)
    }

    #[test]
    fn a_goal_met_after_the_idle_read_shows_as_achieved() {
        let screen = crate::detect::activity::background_activity(
            Some(crate::detect::Agent::Claude),
            SCREEN_GOAL_MET,
        )
        .unwrap();
        for (name, transcript) in MET_GOALS {
            let lines = transcript.lines().collect::<Vec<_>>();
            let met = lines
                .iter()
                .rposition(|line| line.contains("\"goal_status\"") && line.contains("\"met\":true"))
                .unwrap();
            let last_message = lines[..met]
                .iter()
                .rposition(|line| {
                    line.starts_with("{\"type\":\"assistant\"")
                        || line.contains(",\"type\":\"assistant\"")
                })
                .unwrap();
            // The screen goes idle with the last message; the read follows.
            let read_at = line_millis(lines[last_message]).unwrap() + SETTLE.as_millis() as u64;
            assert!(
                line_millis(lines[met]).unwrap() > read_at,
                "{name}: met before the read"
            );
            let written = lines
                .iter()
                .take_while(|line| line_millis(line).is_none_or(|at| at <= read_at))
                .count();
            assert!(written <= met, "{name}");

            let path = temp_transcript(&format!("goal-{}", name.replace(' ', "-")));
            std::fs::write(&path, lines[..written].join("\n") + "\n").unwrap();
            let mut watch = ClaudeTranscriptWatch::new(path.clone());
            watch.tracker = advance(&path, ClaudeLoopTracker::starting_at(0));
            watch.goal = watch.tracker.goal();
            assert_eq!(
                watch.goal,
                Some(TranscriptGoal::Active),
                "{name}: read before the met record"
            );

            let info = crate::api::schema::BackgroundActivityInfo::from_activity(
                &screen,
                Some(&watch),
                None,
            )
            .unwrap();
            assert!(info.goal_achieved && !info.goal, "{name}: {info:?}");

            // The next read finds the met record and agrees.
            std::fs::write(&path, transcript).unwrap();
            assert_eq!(
                advance(&path, watch.tracker).goal(),
                Some(TranscriptGoal::Achieved),
                "{name}"
            );
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
    }

    #[test]
    fn local_clock_formats_hours_and_minutes() {
        let clock = local_clock(1_761_282_000).unwrap();
        assert_eq!(clock.len(), 5);
        assert_eq!(&clock[2..3], ":");
    }
}
