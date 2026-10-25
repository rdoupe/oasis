//! Codex CLI's subagents, read from its session transcripts.
//!
//! Codex shows a spawned subagent only as a `• Spawned <id>` line in the
//! conversation, so the screen cannot count them. Its transcripts can: the
//! parent's records each `spawn_agent` and `close_agent` call as a
//! `CollabAgentToolCall` item naming the subagents' thread ids, and each
//! subagent writes a transcript of its own whose turns start
//! (`task_started`) and end (`task_complete`, `turn_aborted`) like any
//! session's. A subagent runs while its last turn is open. It can finish
//! without waking the parent, so callers check again while any run.
//!
//! Transcripts live at `<codex home>/sessions/YYYY/MM/DD/rollout-…-<id>.jsonl`,
//! the date the thread started; its id is a UUIDv7, which carries that time.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A subagent whose open turn has written nothing for this long died with an
/// earlier Codex process rather than still running.
const STALE_AFTER: Duration = Duration::from_secs(30 * 60);
/// How far back from a subagent transcript's end to look for its last turn.
const TAIL_LIMIT: u64 = 4 * 1024 * 1024;
const TAIL_CHUNK: u64 = 64 * 1024;

/// A pane's Codex thread and the subagents it spawned. Shared runtime state:
/// it rides a server handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexSubagentWatch {
    /// The parent thread's transcript.
    pub path: PathBuf,
    /// Bytes of it already parsed.
    pub offset: u64,
    /// Subagent thread ids in spawn order, until closed.
    pub subagents: Vec<String>,
    /// How many of them run; `None` until read.
    pub running: Option<u16>,
    /// A read is in flight on another thread.
    #[serde(skip)]
    pub scanning: bool,
    /// Something changed while a read was in flight; read again after it.
    #[serde(skip)]
    pub rescan: bool,
    /// Each subagent's transcript, its size when last read, and whether its
    /// last turn was open then, so an unchanged one is not read again.
    #[serde(skip)]
    pub seen: HashMap<String, SubagentSeen>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentSeen {
    path: PathBuf,
    len: u64,
    open: bool,
}

impl CodexSubagentWatch {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            offset: 0,
            subagents: Vec::new(),
            running: None,
            scanning: false,
            rescan: false,
            seen: HashMap::new(),
        }
    }

    /// Feeds parent transcript bytes read from `offset`. Only complete lines
    /// are parsed; returns how many bytes were consumed.
    pub fn feed(&mut self, bytes: &[u8]) -> usize {
        let end = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        for line in bytes[..end].split(|byte| *byte == b'\n') {
            self.feed_line(line);
        }
        self.offset += end as u64;
        end
    }

    fn feed_line(&mut self, line: &[u8]) {
        if !contains(line, b"\"CollabAgentToolCall\"") {
            return;
        }
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        let Some(item) = entry
            .pointer("/payload/item")
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("CollabAgentToolCall"))
        else {
            return;
        };
        let ids = item
            .get("receiver_thread_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|id| is_thread_id(id));
        match item.get("tool").and_then(Value::as_str) {
            Some("spawn_agent") => {
                for id in ids {
                    if !self.subagents.iter().any(|known| known == id) {
                        self.subagents.push(id.to_owned());
                    }
                }
            }
            Some("close_agent") => {
                let closed = ids.collect::<Vec<_>>();
                self.subagents.retain(|id| !closed.contains(&id.as_str()));
            }
            _ => {}
        }
    }

    /// Reads what the parent transcript gained, then how many subagents run.
    /// `sessions` is the Codex home's `sessions` folder.
    pub fn advance(&mut self, sessions: &Path, now: SystemTime) {
        let path = self.path.clone();
        read_new_lines(&path, self);
        let mut running = 0u16;
        for id in &self.subagents {
            let seen = self.seen.get(id).cloned();
            let path = match seen.as_ref() {
                Some(seen) => Some(seen.path.clone()),
                None => transcript_for_thread(sessions, id),
            };
            let Some(path) = path else {
                // Not written yet: a subagent that just spawned.
                running = running.saturating_add(1);
                continue;
            };
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            let open = match seen {
                Some(seen) if seen.len == metadata.len() => seen.open,
                _ => last_turn_open(&path, metadata.len()),
            };
            self.seen.insert(
                id.clone(),
                SubagentSeen {
                    path,
                    len: metadata.len(),
                    open,
                },
            );
            let stale = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|idle| idle > STALE_AFTER);
            if open && !stale {
                running = running.saturating_add(1);
            }
        }
        self.seen.retain(|id, _| self.subagents.contains(id));
        self.running = Some(running);
    }
}

fn read_new_lines(path: &Path, watch: &mut CodexSubagentWatch) {
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    let len = file.metadata().map_or(0, |metadata| metadata.len());
    if len < watch.offset {
        watch.offset = 0;
        watch.subagents.clear();
    }
    while watch.offset < len {
        let mut chunk = Vec::new();
        let read = file
            .seek(SeekFrom::Start(watch.offset))
            .and_then(|_| (&mut file).take(TAIL_LIMIT).read_to_end(&mut chunk));
        let Ok(read) = read else {
            return;
        };
        if read == 0 {
            return;
        }
        if watch.feed(&chunk) == 0 {
            if (read as u64) < TAIL_LIMIT {
                return; // A line still being written.
            }
            // One line longer than a read holds no spawn record.
            watch.offset += read as u64;
        }
    }
}

/// Whether a subagent transcript's last turn event opens a turn.
fn last_turn_open(path: &Path, len: u64) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut end = len;
    while end > 0 && len - end < TAIL_LIMIT {
        // Overlap reads so an event split across two of them is still seen.
        let start = end.saturating_sub(TAIL_CHUNK);
        let mut chunk = Vec::new();
        if file
            .seek(SeekFrom::Start(start))
            .and_then(|_| (&mut file).take(end - start + 256).read_to_end(&mut chunk))
            .is_err()
        {
            return false;
        }
        let last = |needle: &[u8]| rfind(&chunk, needle);
        let started = last(b"{\"type\":\"task_started\"");
        let ended = last(b"{\"type\":\"task_complete\"").max(last(b"{\"type\":\"turn_aborted\""));
        match (started, ended) {
            (Some(started), Some(ended)) => return started > ended,
            (Some(_), None) => return true,
            (None, Some(_)) => return false,
            (None, None) => end = start,
        }
    }
    // A turn so long its start is out of reach is still running.
    len > 0
}

/// `<sessions>/YYYY/MM/DD/rollout-…-<id>.jsonl`, looked up in the folders for
/// the UTC day the id's time falls on and the days either side, which covers
/// any local time zone.
pub fn transcript_for_thread(sessions: &Path, id: &str) -> Option<PathBuf> {
    if !is_thread_id(id) {
        return None;
    }
    let millis = i64::from_str_radix(&id.replace('-', "")[..12], 16).ok()?;
    let started = time::OffsetDateTime::from_unix_timestamp(millis / 1000).ok()?;
    let suffix = format!("-{id}.jsonl");
    [-1i64, 0, 1].into_iter().find_map(|days| {
        let day = started.checked_add(time::Duration::days(days))?;
        let dir = sessions
            .join(format!("{:04}", day.year()))
            .join(format!("{:02}", u8::from(day.month())))
            .join(format!("{:02}", day.day()));
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("rollout-") && name.ends_with(&suffix))
            })
    })
}

/// A UUID: 32 hex digits in 8-4-4-4-12 groups.
fn is_thread_id(id: &str) -> bool {
    let groups = id.split('-').collect::<Vec<_>>();
    groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && groups
            .iter()
            .all(|group| group.chars().all(|c| c.is_ascii_hexdigit()))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    rfind(haystack, needle).is_some()
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PARENT: &str = "01a1753b-cd5d-7900-9d3f-3251e0833832";
    const CHILD: &str = "01a1753c-06df-75f0-8e36-46dbcea2c2cf";

    // Shapes captured from a live Codex CLI 0.164.0 session.
    fn spawn(id: &str) -> String {
        format!(
            r#"{{"type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"CollabAgentToolCall","tool":"spawn_agent","status":"completed","sender_thread_id":"{PARENT}","receiver_thread_ids":["{id}"]}}}}}}"#
        )
    }
    fn close(id: &str) -> String {
        format!(
            r#"{{"type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"CollabAgentToolCall","tool":"close_agent","status":"completed","receiver_thread_ids":["{id}"]}}}}}}"#
        )
    }
    const STARTED: &str =
        r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#;
    const COMPLETE: &str = r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","last_agent_message":"done"}}"#;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oasis-codex-subagents-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_child(sessions: &Path, lines: &[&str]) -> PathBuf {
        let dir = sessions.join("2026/10/25");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-10-25T18-43-50-{CHILD}.jsonl"));
        let text = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn counts_spawned_subagents_while_their_turn_is_open() {
        let root = scratch("count");
        let sessions = root.join("sessions");
        let parent = root.join("parent.jsonl");
        std::fs::write(&parent, format!("{}\n", spawn(CHILD))).unwrap();
        let mut watch = CodexSubagentWatch::new(parent.clone());
        let now = SystemTime::now();
        watch.advance(&sessions, now);
        assert_eq!(
            watch.running,
            Some(1),
            "spawned, transcript not written yet"
        );

        write_child(&sessions, &[STARTED]);
        watch.advance(&sessions, now);
        assert_eq!(watch.running, Some(1));

        write_child(&sessions, &[STARTED, COMPLETE]);
        watch.advance(&sessions, now);
        assert_eq!(watch.running, Some(0), "finished, though not closed");

        write_child(&sessions, &[STARTED, COMPLETE, STARTED]);
        watch.advance(&sessions, now);
        assert_eq!(watch.running, Some(1), "a new message started another turn");

        std::fs::write(&parent, format!("{}\n{}\n", spawn(CHILD), close(CHILD))).unwrap();
        watch.advance(&sessions, now);
        assert_eq!((watch.running, watch.subagents.len()), (Some(0), 0));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_open_turn_left_by_a_dead_process_goes_stale() {
        let root = scratch("stale");
        let sessions = root.join("sessions");
        let parent = root.join("parent.jsonl");
        std::fs::write(&parent, format!("{}\n", spawn(CHILD))).unwrap();
        write_child(&sessions, &[STARTED]);
        let mut watch = CodexSubagentWatch::new(parent);
        watch.advance(&sessions, SystemTime::now() + STALE_AFTER * 2);
        assert_eq!(watch.running, Some(0));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finds_a_thread_transcript_by_the_time_in_its_id() {
        let root = scratch("find");
        let sessions = root.join("sessions");
        let path = write_child(&sessions, &[STARTED]);
        assert_eq!(transcript_for_thread(&sessions, CHILD), Some(path));
        assert_eq!(transcript_for_thread(&sessions, PARENT), None);
        assert_eq!(transcript_for_thread(&sessions, "../../etc"), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prose_naming_the_tool_spawns_nothing() {
        let mut watch = CodexSubagentWatch::new(PathBuf::from("/nonexistent"));
        watch.feed(
            br#"{"type":"response_item","payload":{"type":"message","content":[{"type":"output_text","text":"CollabAgentToolCall spawn_agent 01a1753c-06df-75f0-8e36-46dbcea2c2cf"}]}}
"#,
        );
        assert!(watch.subagents.is_empty());
    }
}
