//! Replays the Claude Code corpus: anonymized excerpts of real sessions, each
//! with what Oasis should read from it (see `scripts/claude_corpus.py`).
//!
//! A case's transcript is read the way a pane's watch reads it, a piece at a
//! time; its subagent records sit beside it as Claude keeps them; its screen
//! goes through the screen reader; and what the API would show combines them.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{advance, unfinished_subagents};
use crate::api::schema::BackgroundActivityInfo;
use crate::detect::activity::{background_activity, BackgroundActivity};
use crate::detect::claude_loop::{
    ClaudeLoopTracker, ClaudeTranscriptWatch, LoopStatus, TranscriptGoal,
};
use crate::detect::Agent;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    /// The Claude Code version the session ran.
    claude: String,
    about: String,
    #[serde(default)]
    read: Vec<Read>,
    /// Subagents whose records beside the transcript are not finished.
    unfinished_subagents: Option<u16>,
    screen: Option<Screen>,
    shown: Option<Shown>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    /// 1-based; the whole excerpt when omitted.
    after_line: Option<usize>,
    goal: String,
    #[serde(rename = "loop")]
    looping: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Screen {
    shells: u16,
    monitors: u16,
    agents: u16,
    mcp_tasks: u16,
    goal: bool,
    goal_achieved: bool,
    goal_failed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Shown {
    /// Which read (1-based) the transcript stands at; the last when omitted.
    after_read: Option<usize>,
    /// Another case whose screen this session showed alike.
    screen_from: Option<String>,
    #[serde(default)]
    goal: bool,
    #[serde(default)]
    goal_achieved: bool,
    #[serde(default)]
    goal_failed: bool,
    #[serde(default)]
    agents: u16,
    #[serde(default)]
    shells: u16,
    #[serde(default)]
    monitors: u16,
}

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-corpus")
}

fn goal_name(goal: Option<TranscriptGoal>) -> &'static str {
    match goal {
        None => "unknown",
        Some(TranscriptGoal::Active) => "active",
        Some(TranscriptGoal::Achieved) => "achieved",
        Some(TranscriptGoal::Failed) => "failed",
        Some(TranscriptGoal::None) => "none",
    }
}

fn loop_name(status: Option<LoopStatus>) -> String {
    match status {
        None => "none".to_owned(),
        Some(LoopStatus::Active) => "running".to_owned(),
        Some(LoopStatus::NextWakeup(_)) => "wakeup".to_owned(),
        Some(LoopStatus::Interval(interval)) => format!("every {interval}"),
    }
}

fn read_screen(name: &str) -> Result<BackgroundActivity, String> {
    let path = corpus().join(name).join("screen.txt");
    let text =
        std::fs::read_to_string(&path).map_err(|err| format!("{}: {err}", path.display()))?;
    background_activity(Some(Agent::Claude), &text).ok_or_else(|| "no footer to read".to_owned())
}

/// Checks one case; returns what did not match.
fn check(name: &str, dir: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    let mut fail = |what: String| failures.push(format!("{name}: {what}"));
    let case: Case = match std::fs::read_to_string(dir.join("case.toml"))
        .map_err(|err| err.to_string())
        .and_then(|text| toml::from_str(&text).map_err(|err| err.to_string()))
    {
        Ok(case) => case,
        Err(err) => {
            fail(format!("case.toml: {err}"));
            return failures;
        }
    };
    if case.claude.is_empty() || case.about.is_empty() || case.about.starts_with("TODO") {
        fail("case.toml needs `claude` and `about`".to_owned());
    }
    if case.read.is_empty() && case.screen.is_none() && case.shown.is_none() {
        fail("case.toml checks nothing".to_owned());
    }

    // The transcript, read a piece at a time as a watch reads it.
    let mut watches = Vec::new();
    if !case.read.is_empty() {
        let transcript = dir.join("transcript.jsonl");
        let lines = match std::fs::read_to_string(&transcript) {
            Ok(text) => text.lines().map(str::to_owned).collect::<Vec<_>>(),
            Err(err) => {
                fail(format!("transcript.jsonl: {err}"));
                return failures;
            }
        };
        let scratch =
            std::env::temp_dir().join(format!("oasis-claude-corpus-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        let path = scratch.join("session.jsonl");
        let mut tracker = ClaudeLoopTracker::starting_at(0);
        for (index, read) in case.read.iter().enumerate() {
            let upto = read.after_line.unwrap_or(lines.len());
            if upto > lines.len() {
                fail(format!(
                    "read {}: after_line {upto} is past the excerpt's end",
                    index + 1
                ));
                continue;
            }
            std::fs::write(
                &path,
                lines[..upto]
                    .iter()
                    .map(|line| format!("{line}\n"))
                    .collect::<String>(),
            )
            .expect("write excerpt");
            tracker = advance(&path, tracker);
            let goal = goal_name(tracker.goal());
            let looping = loop_name(tracker.status());
            if goal != read.goal {
                fail(format!(
                    "read {} (line {upto}): goal is {goal}, expected {}",
                    index + 1,
                    read.goal
                ));
            }
            if looping != read.looping {
                fail(format!(
                    "read {} (line {upto}): loop is {looping}, expected {}",
                    index + 1,
                    read.looping
                ));
            }
            let mut read_watch = ClaudeTranscriptWatch::new(transcript.clone());
            read_watch.goal = tracker.goal();
            read_watch.loop_label = tracker.status().map(|_| String::new());
            watches.push(read_watch);
        }
        let _ = std::fs::remove_dir_all(&scratch);
    }

    if let Some(expected) = case.unfinished_subagents {
        // A session seen only through its subagent records and screen.
        if watches.is_empty() {
            watches.push(ClaudeTranscriptWatch::new(dir.join("transcript.jsonl")));
        }
        let found = unfinished_subagents(&dir.join("transcript.jsonl"));
        if found != Some(expected) {
            fail(format!(
                "unfinished subagents: {found:?}, expected {expected}"
            ));
        }
        for watch in &mut watches {
            watch.unfinished_subagents = found;
        }
    }

    if let Some(expected) = &case.screen {
        match read_screen(name) {
            Ok(found) => {
                let expected = BackgroundActivity {
                    shells: expected.shells,
                    monitors: expected.monitors,
                    agents: expected.agents,
                    mcp_tasks: expected.mcp_tasks,
                    goal: expected.goal,
                    goal_achieved: expected.goal_achieved,
                    goal_failed: expected.goal_failed,
                    ..BackgroundActivity::default()
                };
                if found != expected {
                    fail(format!("screen reads {found:?}, expected {expected:?}"));
                }
            }
            Err(err) => fail(format!("screen: {err}")),
        }
    }

    if let Some(shown) = &case.shown {
        let screen = read_screen(shown.screen_from.as_deref().unwrap_or(name));
        let at = shown.after_read.unwrap_or(watches.len());
        let watch = match at.checked_sub(1).map(|index| watches.get(index)) {
            _ if watches.is_empty() => None,
            Some(Some(watch)) => Some(watch),
            _ => {
                fail(format!("shown: after_read {at} names no read"));
                None
            }
        };
        match screen {
            Ok(screen) => {
                let found =
                    BackgroundActivityInfo::from_activity(&screen, watch, None).unwrap_or_default();
                let summary = (
                    found.goal,
                    found.goal_achieved,
                    found.goal_failed,
                    found.agents,
                    found.shells,
                    found.monitors,
                );
                let expected = (
                    shown.goal,
                    shown.goal_achieved,
                    shown.goal_failed,
                    shown.agents,
                    shown.shells,
                    shown.monitors,
                );
                if summary != expected {
                    fail(format!(
                        "shown (goal, achieved, failed, agents, shells, monitors) is {summary:?}, expected {expected:?}"
                    ));
                }
            }
            Err(err) => fail(format!("shown: {err}")),
        }
    }
    failures
}

#[test]
fn claude_corpus() {
    let mut cases = std::fs::read_dir(corpus())
        .expect("claude corpus")
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.path(),
            )
        })
        .collect::<Vec<_>>();
    cases.sort();
    assert!(!cases.is_empty(), "the corpus has no cases");
    let failures = cases
        .iter()
        .flat_map(|(name, dir)| check(name, dir))
        .collect::<Vec<_>>();
    assert!(
        failures.is_empty(),
        "{} of {} corpus cases disagree:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}
