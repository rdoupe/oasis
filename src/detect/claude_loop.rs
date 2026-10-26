//! Claude Code's loops and goals, read from its session transcript.
//!
//! Stock Claude Code draws nothing on screen for a `/loop`, so the transcript
//! is the only place to see one. A self-paced loop is a chain of
//! `ScheduleWakeup` calls: each arms the next wakeup, the wakeup fires
//! (`scheduled_task_fire`) and starts a turn, and a turn that ends
//! (`turn_duration`) without arming another wakeup ends the loop. A fixed
//! interval loop is a recurring `CronCreate` job that runs until a
//! `CronDelete` names its id. Wakeups and session-only jobs live in the Claude
//! process, so a watch starts at the end of a resumed transcript.
//!
//! A `/goal` writes `goal_status` attachments: `met: false` while it runs,
//! `met: true` with a reason once achieved, `met: false` marked `failed` once
//! Claude gives up on it, and `met: true` marked `sentinel` when it is cleared
//! without being met. An achieved or failed goal lasts until the next prompt,
//! as the screen shows it.
//!
//! The tracker only parses bytes it is fed; the caller reads the file.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Loop state carried across incremental reads of one transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeLoopTracker {
    /// Bytes of the transcript already parsed; `None` until the first read,
    /// which starts at the end of an existing transcript.
    pub offset: Option<u64>,
    wakeup: Option<Wakeup>,
    jobs: Vec<CronJob>,
    /// `None` until a goal record is read; earlier goals are unknown.
    #[serde(default)]
    goal: Option<TranscriptGoal>,
}

/// A goal as the transcript last recorded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptGoal {
    Active,
    /// Met in the last turn; lasts until the next prompt.
    Achieved,
    /// Given up on in the last turn; lasts until the next prompt.
    Failed,
    /// Cleared, or achieved before the current prompt.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Wakeup {
    /// Unix seconds of the armed wakeup; `None` while a turn it started runs.
    next_at: Option<i64>,
    /// A wakeup fired or `/loop` started a turn that has not ended yet.
    ticking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CronJob {
    tool_use_id: String,
    job_id: Option<String>,
    interval: Option<String>,
}

/// A pane's Claude transcript and what has been read from it. Shared runtime
/// state: it rides a server handoff so a live loop stays visible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeTranscriptWatch {
    pub path: std::path::PathBuf,
    pub tracker: ClaudeLoopTracker,
    /// `None` without a loop; otherwise its next wakeup (`22:42`) or interval
    /// (`2m`), empty while a loop turn runs or the schedule is unknown.
    pub loop_label: Option<String>,
    /// The goal the transcript records; `None` while unknown.
    #[serde(default)]
    pub goal: Option<TranscriptGoal>,
    /// Subagents of this session not yet finished, from their records beside
    /// the transcript; `None` while unread.
    #[serde(default)]
    pub unfinished_subagents: Option<u16>,
    /// Which goal records the reader knew when it read them (see
    /// [`GOAL_READER`]); a watch handed over from an older build may have
    /// misread ones it did not know.
    #[serde(default)]
    pub goal_reader: u8,
    /// A read is in flight on another thread.
    #[serde(skip)]
    pub scanning: bool,
    /// Something changed while a read was in flight; read again after it.
    #[serde(skip)]
    pub rescan: bool,
}

impl ClaudeTranscriptWatch {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self {
            path,
            tracker: ClaudeLoopTracker::default(),
            loop_label: None,
            goal: None,
            unfinished_subagents: None,
            goal_reader: GOAL_READER,
            scanning: false,
            rescan: false,
        }
    }

    /// A watch handed over from a build that did not know every goal record
    /// forgets the goal it read, until the next goal record; the screen shows
    /// the goal meanwhile.
    pub fn forget_goal_read_by_an_older_build(&mut self) {
        if self.goal_reader < GOAL_READER {
            self.goal = None;
            self.tracker.goal = None;
            self.goal_reader = GOAL_READER;
        }
    }
}

/// Bumped when the reader learns a goal record it used to misread: 1 knows
/// goals Claude gives up on.
pub const GOAL_READER: u8 = 1;

/// What the sidebar shows for an active loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopStatus {
    /// The next self-paced wakeup, in Unix seconds.
    NextWakeup(i64),
    /// A fixed interval job, e.g. `2m`.
    Interval(String),
    /// A loop is running a turn or its schedule is unknown.
    Active,
}

impl ClaudeLoopTracker {
    /// A tracker with nothing read, continuing from `offset`.
    pub fn starting_at(offset: u64) -> Self {
        Self {
            offset: Some(offset),
            ..Self::default()
        }
    }

    /// Feeds bytes read from `offset`. Only complete lines are parsed; returns
    /// how many bytes were consumed, and the offset advances by as much.
    pub fn feed(&mut self, bytes: &[u8]) -> usize {
        let end = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        for line in bytes[..end].split(|byte| *byte == b'\n') {
            self.feed_line(line);
        }
        self.offset = Some(self.offset.unwrap_or(0) + end as u64);
        end
    }

    pub fn status(&self) -> Option<LoopStatus> {
        if let Some(wakeup) = &self.wakeup {
            return Some(match wakeup.next_at {
                Some(next_at) if !wakeup.ticking => LoopStatus::NextWakeup(next_at),
                _ => LoopStatus::Active,
            });
        }
        let job = self.jobs.first()?;
        Some(
            job.interval
                .clone()
                .map_or(LoopStatus::Active, LoopStatus::Interval),
        )
    }

    pub fn goal(&self) -> Option<TranscriptGoal> {
        self.goal
    }

    fn feed_line(&mut self, line: &[u8]) {
        // Only an ended goal needs every prompt: the next one clears it.
        let prompt_ends_goal =
            self.goal_ended() && contains(&line[..line.len().min(160)], b"\"type\":\"user\"");
        if !prompt_ends_goal && !RELEVANT.iter().any(|needle| contains(line, needle)) {
            return;
        }
        // Request blobs copy the whole system prompt, tool docs included.
        if contains(&line[..line.len().min(48)], b"\"type\":\"api-request") {
            return;
        }
        let Ok(entry) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        let timestamp = entry
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(unix_seconds);
        match entry.get("type").and_then(Value::as_str) {
            Some("system") => self.system_entry(&entry),
            Some("attachment") => self.attachment_entry(&entry),
            Some(kind @ ("user" | "assistant")) => {
                if kind == "user" {
                    self.prompt_entry(&entry);
                }
                self.message_entry(&entry, timestamp);
            }
            _ => {}
        }
    }

    fn attachment_entry(&mut self, entry: &Value) {
        let Some(attachment) = entry.get("attachment").filter(|attachment| {
            attachment.get("type").and_then(Value::as_str) == Some("goal_status")
        }) else {
            return;
        };
        let flag = |name: &str| attachment.get(name).and_then(Value::as_bool) == Some(true);
        self.goal = Some(match (flag("met"), flag("sentinel")) {
            (false, _) if flag("failed") => TranscriptGoal::Failed,
            (false, _) => TranscriptGoal::Active,
            (true, false) => TranscriptGoal::Achieved,
            (true, true) => TranscriptGoal::None,
        });
    }

    fn goal_ended(&self) -> bool {
        matches!(
            self.goal,
            Some(TranscriptGoal::Achieved | TranscriptGoal::Failed)
        )
    }

    /// A prompt typed after a goal was achieved or failed ends its notice.
    /// Prompts Claude makes itself (a task notification, a scheduled loop
    /// turn, the summary an idle compaction writes) are not seen by anyone.
    fn prompt_entry(&mut self, entry: &Value) {
        let flag = |name: &str| entry.get(name).and_then(Value::as_bool) == Some(true);
        if !self.goal_ended() || flag("isMeta") || flag("isCompactSummary") {
            return;
        }
        let Some(text) = entry.pointer("/message/content").and_then(Value::as_str) else {
            return;
        };
        let typed = match entry.pointer("/origin/kind").and_then(Value::as_str) {
            Some(kind) => kind == "human",
            // Older versions record no origin.
            None => {
                entry.get("scheduledTaskId").is_none() && !text.starts_with("<task-notification>")
            }
        };
        if typed {
            self.goal = Some(TranscriptGoal::None);
        }
    }

    fn system_entry(&mut self, entry: &Value) {
        match entry.get("subtype").and_then(Value::as_str) {
            Some("turn_duration") => {
                if self.wakeup.as_ref().is_some_and(|wakeup| wakeup.ticking) {
                    self.wakeup = None;
                }
            }
            Some("informational")
                if entry
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| content.starts_with("Goal cleared")) =>
            {
                self.goal = Some(TranscriptGoal::None);
            }
            Some("scheduled_task_fire") => {
                let is_wakeup = entry.get("taskKind").and_then(Value::as_str) == Some("loop");
                if let Some(wakeup) = self.wakeup.as_mut().filter(|_| is_wakeup) {
                    wakeup.ticking = true;
                    wakeup.next_at = None;
                }
            }
            _ => {}
        }
    }

    fn message_entry(&mut self, entry: &Value, timestamp: Option<i64>) {
        let Some(content) = entry.pointer("/message/content") else {
            return;
        };
        if let Some(text) = content.as_str() {
            if text.contains("<command-name>/loop</command-name>") {
                self.wakeup = Some(Wakeup {
                    next_at: None,
                    ticking: true,
                });
            }
            return;
        }
        for block in content.as_array().into_iter().flatten() {
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => self.tool_use(block, timestamp),
                Some("tool_result") => self.tool_result(block),
                _ => {}
            }
        }
    }

    fn tool_use(&mut self, block: &Value, timestamp: Option<i64>) {
        let input = block.get("input");
        let field = |name: &str| input.and_then(|input| input.get(name));
        match block.get("name").and_then(Value::as_str) {
            Some("ScheduleWakeup") => {
                if field("stop").and_then(Value::as_bool) == Some(true) {
                    self.wakeup = None;
                    return;
                }
                let delay = field("delaySeconds").and_then(Value::as_f64).unwrap_or(0.0);
                self.wakeup = Some(Wakeup {
                    next_at: timestamp.map(|at| at + delay as i64),
                    ticking: false,
                });
            }
            Some("CronCreate") => {
                if field("recurring").and_then(Value::as_bool) == Some(false) {
                    return;
                }
                let Some(id) = block.get("id").and_then(Value::as_str) else {
                    return;
                };
                if self.jobs.iter().any(|job| job.tool_use_id == id) {
                    return;
                }
                self.jobs.push(CronJob {
                    tool_use_id: id.to_owned(),
                    job_id: None,
                    interval: field("cron")
                        .and_then(Value::as_str)
                        .and_then(cron_interval),
                });
            }
            Some("CronDelete") => {
                if let Some(id) = field("id").and_then(Value::as_str) {
                    self.jobs.retain(|job| job.job_id.as_deref() != Some(id));
                }
            }
            _ => {}
        }
    }

    fn tool_result(&mut self, block: &Value) {
        let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
            return;
        };
        let Some(index) = self.jobs.iter().position(|job| job.tool_use_id == id) else {
            return;
        };
        if block.get("is_error").and_then(Value::as_bool) == Some(true) {
            self.jobs.remove(index);
            return;
        }
        let text = match block.get("content") {
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
            None => return,
        };
        // "Scheduled recurring job 684f598e (Every 2 minutes). Next run: …"
        self.jobs[index].job_id = text
            .split_once("job ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .map(|id| {
                id.trim_matches(|c: char| !c.is_ascii_alphanumeric())
                    .to_owned()
            })
            .filter(|id| !id.is_empty());
    }
}

const RELEVANT: [&[u8]; 8] = [
    b"/loop",
    b"ScheduleWakeup",
    b"turn_duration",
    b"scheduled_task_fire",
    b"Cron",
    b"job ",
    b"goal_status",
    b"Goal cleared",
];

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Unix seconds of an RFC 3339 UTC timestamp like `2026-10-24T04:55:29.094Z`.
fn unix_seconds(timestamp: &str) -> Option<i64> {
    let number = |range: std::ops::Range<usize>| timestamp.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let month = time::Month::try_from(u8::try_from(month).ok()?).ok()?;
    let date =
        time::Date::from_calendar_date(i32::try_from(year).ok()?, month, u8::try_from(day).ok()?)
            .ok()?;
    let time = time::Time::from_hms(
        u8::try_from(hour).ok()?,
        u8::try_from(minute).ok()?,
        u8::try_from(second).ok()?,
    )
    .ok()?;
    Some(
        time::PrimitiveDateTime::new(date, time)
            .assume_utc()
            .unix_timestamp(),
    )
}

/// A short interval for the cron shapes `/loop` writes: `*/5 * * * *` is
/// `5m`, `0 */2 * * *` is `2h`, `0 * * * *` is `1h`.
fn cron_interval(cron: &str) -> Option<String> {
    let fields = cron.split_whitespace().collect::<Vec<_>>();
    let rest_any = |from: usize| fields[from..].iter().all(|field| *field == "*");
    if fields.len() != 5 {
        return None;
    }
    let every = |field: &str| field.strip_prefix("*/").and_then(|n| n.parse::<u32>().ok());
    if fields[0] == "*" && rest_any(1) {
        return Some("1m".to_owned());
    }
    if let Some(minutes) = every(fields[0]).filter(|_| rest_any(1)) {
        return Some(format!("{minutes}m"));
    }
    if fields[0].parse::<u32>().is_ok() && rest_any(2) {
        if fields[1] == "*" {
            return Some("1h".to_owned());
        }
        if let Some(hours) = every(fields[1]) {
            return Some(format!("{hours}h"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(tracker: &mut ClaudeLoopTracker, lines: &[&str]) {
        let text = lines
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        tracker.feed(text.as_bytes());
    }

    // Shapes captured from a stock Claude Code 2.1.309 transcript.
    const LOOP_COMMAND: &str = r#"{"type":"user","timestamp":"2026-10-24T04:55:26.138Z","message":{"role":"user","content":"<command-message>loop</command-message>\n<command-name>/loop</command-name>\n<command-args>keep checking</command-args>"}}"#;
    const WAKEUP: &str = r#"{"type":"assistant","timestamp":"2026-10-24T04:55:29.094Z","message":{"content":[{"type":"tool_use","id":"toolu_5owbxd","name":"ScheduleWakeup","input":{"delaySeconds":300,"reason":"recheck","prompt":"/loop keep checking"}}]}}"#;
    const TURN_END: &str = r#"{"type":"system","subtype":"turn_duration","durationMs":4112,"timestamp":"2026-10-24T04:55:30.225Z"}"#;
    const WAKEUP_FIRE: &str = r#"{"type":"system","subtype":"scheduled_task_fire","taskKind":"loop","timestamp":"2026-10-24T05:00:29.000Z"}"#;

    #[test]
    fn self_paced_loop_arms_fires_and_ends_without_a_new_wakeup() {
        let mut tracker = ClaudeLoopTracker::default();
        feed(&mut tracker, &[LOOP_COMMAND]);
        assert_eq!(tracker.status(), Some(LoopStatus::Active));
        feed(&mut tracker, &[WAKEUP, TURN_END]);
        let armed_at = unix_seconds("2026-10-24T04:55:29Z").unwrap();
        assert_eq!(
            tracker.status(),
            Some(LoopStatus::NextWakeup(armed_at + 300))
        );
        feed(&mut tracker, &[WAKEUP_FIRE]);
        assert_eq!(tracker.status(), Some(LoopStatus::Active));
        feed(&mut tracker, &[TURN_END]);
        assert_eq!(tracker.status(), None);
    }

    #[test]
    fn a_loop_turn_that_schedules_nothing_ends_the_loop() {
        let mut tracker = ClaudeLoopTracker::default();
        feed(&mut tracker, &[LOOP_COMMAND, TURN_END]);
        assert_eq!(tracker.status(), None);
    }

    #[test]
    fn stopping_the_wakeup_chain_ends_the_loop() {
        let mut tracker = ClaudeLoopTracker::default();
        feed(&mut tracker, &[WAKEUP]);
        feed(
            &mut tracker,
            &[
                r#"{"type":"assistant","timestamp":"2026-10-24T05:00:31.000Z","message":{"content":[{"type":"tool_use","id":"t2","name":"ScheduleWakeup","input":{"stop":true}}]}}"#,
            ],
        );
        assert_eq!(tracker.status(), None);
    }

    #[test]
    fn fixed_interval_loop_lasts_until_its_job_is_deleted() {
        let mut tracker = ClaudeLoopTracker::default();
        feed(
            &mut tracker,
            &[
                r#"{"type":"user","timestamp":"2026-10-24T04:53:37.429Z","message":{"content":"<command-name>/loop</command-name>\n<command-args>2m say tick</command-args>"}}"#,
                r#"{"type":"assistant","timestamp":"2026-10-24T04:53:40.179Z","message":{"content":[{"type":"tool_use","id":"toolu_UUYeMu","name":"CronCreate","input":{"cron":"*/2 * * * *","prompt":"say tick","recurring":true}}]}}"#,
                r#"{"type":"user","timestamp":"2026-10-24T04:53:40.300Z","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_UUYeMu","content":"Scheduled recurring job 684f598e (Every 2 minutes). Next run: Oct 24 12:54am. Session-only."}]}}"#,
                TURN_END,
                r#"{"type":"system","subtype":"scheduled_task_fire","content":"Running scheduled task (Oct 24 12:54am)","taskId":"684f598e","timestamp":"2026-10-24T04:54:25.307Z"}"#,
                TURN_END,
            ],
        );
        assert_eq!(tracker.status(), Some(LoopStatus::Interval("2m".into())));
        feed(
            &mut tracker,
            &[
                r#"{"type":"assistant","timestamp":"2026-10-24T04:54:55.809Z","message":{"content":[{"type":"tool_use","id":"toolu_vAHzCd","name":"CronDelete","input":{"id":"684f598e"}}]}}"#,
            ],
        );
        assert_eq!(tracker.status(), None);
    }

    #[test]
    fn partial_lines_wait_for_the_next_read() {
        let mut tracker = ClaudeLoopTracker::default();
        let consumed = tracker.feed(&WAKEUP.as_bytes()[..40]);
        assert_eq!((consumed, tracker.offset), (0, Some(0)));
        assert_eq!(tracker.status(), None);
        // The caller reads again from the unchanged offset, now whole.
        tracker.feed(format!("{WAKEUP}\n").as_bytes());
        assert!(matches!(tracker.status(), Some(LoopStatus::NextWakeup(_))));
    }

    #[test]
    fn request_blobs_and_prose_cannot_start_a_loop() {
        let mut tracker = ClaudeLoopTracker::default();
        feed(
            &mut tracker,
            &[
                r#"{"type":"api-request-blob","hash":"x","body":"ScheduleWakeup <command-name>/loop</command-name>"}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"I could call ScheduleWakeup or /loop"}]}}"#,
            ],
        );
        assert_eq!(tracker.status(), None);
    }

    // Shapes captured from a stock Claude Code 2.1.310 transcript.
    const GOAL_SET: &str = r#"{"type":"attachment","attachment":{"type":"goal_status","met":false,"sentinel":true,"condition":"deploy it"}}"#;
    const GOAL_CHECK: &str = r#"{"type":"attachment","attachment":{"type":"goal_status","met":false,"condition":"deploy it","reason":"not yet"}}"#;
    const GOAL_MET: &str = r#"{"type":"attachment","attachment":{"type":"goal_status","met":true,"condition":"deploy it","reason":"deployed"}}"#;
    const GOAL_ENDED: &str = r#"{"type":"attachment","attachment":{"type":"goal_status","met":true,"sentinel":true,"condition":"deploy it"}}"#;
    const PROMPT: &str = r#"{"type":"user","message":{"role":"user","content":"thanks"}}"#;
    const TOOL_RESULT: &str = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}}"#;

    #[test]
    fn a_goal_runs_is_achieved_and_ends_at_the_next_prompt() {
        let mut tracker = ClaudeLoopTracker::default();
        assert_eq!(tracker.goal(), None, "unknown until a goal record");
        feed(&mut tracker, &[GOAL_SET, GOAL_CHECK]);
        assert_eq!(tracker.goal(), Some(TranscriptGoal::Active));
        feed(&mut tracker, &[GOAL_MET, TURN_END, TOOL_RESULT]);
        assert_eq!(tracker.goal(), Some(TranscriptGoal::Achieved));
        feed(&mut tracker, &[PROMPT]);
        assert_eq!(tracker.goal(), Some(TranscriptGoal::None));
    }

    #[test]
    fn a_watch_from_an_older_build_forgets_the_goal_it_read() {
        let mut watch = ClaudeTranscriptWatch::new("/session.jsonl".into());
        feed(&mut watch.tracker, &[GOAL_SET]);
        watch.goal = watch.tracker.goal();
        let mut json = serde_json::to_value(&watch).unwrap();
        let mut current: ClaudeTranscriptWatch = serde_json::from_value(json.clone()).unwrap();
        current.forget_goal_read_by_an_older_build();
        assert_eq!(current.goal, Some(TranscriptGoal::Active));

        json.as_object_mut().unwrap().remove("goal_reader");
        let mut older: ClaudeTranscriptWatch = serde_json::from_value(json).unwrap();
        older.forget_goal_read_by_an_older_build();
        assert_eq!((older.goal, older.tracker.goal()), (None, None));
        feed(&mut older.tracker, &[GOAL_CHECK]);
        assert_eq!(older.tracker.goal(), Some(TranscriptGoal::Active));
    }

    #[test]
    fn a_goal_given_up_on_lasts_until_a_typed_prompt() {
        let mut tracker = ClaudeLoopTracker::default();
        feed(
            &mut tracker,
            &[
                GOAL_SET,
                GOAL_CHECK,
                r#"{"type":"attachment","attachment":{"type":"goal_status","met":false,"failed":true,"condition":"deploy it","reason":"cannot"}}"#,
                TURN_END,
                r#"{"type":"user","origin":{"kind":"task-notification"},"message":{"content":"<task-notification>done</task-notification>"}}"#,
            ],
        );
        assert_eq!(tracker.goal(), Some(TranscriptGoal::Failed));
        feed(
            &mut tracker,
            &[r#"{"type":"user","origin":{"kind":"human"},"message":{"content":"why"}}"#],
        );
        assert_eq!(tracker.goal(), Some(TranscriptGoal::None));
    }

    #[test]
    fn only_a_typed_prompt_ends_a_met_goal() {
        let mut tracker = ClaudeLoopTracker::default();
        feed(
            &mut tracker,
            &[
                GOAL_MET,
                r#"{"type":"user","isCompactSummary":true,"message":{"content":"This session is being continued"}}"#,
                r#"{"type":"user","origin":{"kind":"task-notification"},"message":{"content":"<task-notification>done</task-notification>"}}"#,
                r#"{"type":"user","scheduledTaskId":"c1b98c2c","message":{"content":"<command-name>/loop</command-name>"}}"#,
                r#"{"type":"user","message":{"content":"<task-notification>done</task-notification>"}}"#,
            ],
        );
        assert_eq!(tracker.goal(), Some(TranscriptGoal::Achieved));
        feed(
            &mut tracker,
            &[r#"{"type":"user","origin":{"kind":"human"},"message":{"content":"next"}}"#],
        );
        assert_eq!(tracker.goal(), Some(TranscriptGoal::None));
    }

    #[test]
    fn a_cleared_goal_is_no_goal() {
        let mut tracker = ClaudeLoopTracker::default();
        feed(&mut tracker, &[GOAL_SET, GOAL_ENDED]);
        assert_eq!(tracker.goal(), Some(TranscriptGoal::None));
        feed(
            &mut tracker,
            &[
                GOAL_CHECK,
                r#"{"type":"system","subtype":"informational","content":"Goal cleared after an unrecoverable error (authentication failed)"}"#,
            ],
        );
        assert_eq!(tracker.goal(), Some(TranscriptGoal::None));
    }

    #[test]
    fn cron_intervals() {
        for (cron, interval) in [
            ("*/5 * * * *", Some("5m")),
            ("* * * * *", Some("1m")),
            ("7 * * * *", Some("1h")),
            ("0 */2 * * *", Some("2h")),
            ("30 9 * * 1-5", None),
        ] {
            assert_eq!(cron_interval(cron).as_deref(), interval, "{cron}");
        }
    }
}
