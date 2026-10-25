//! Background work an agent reports on screen around its prompt: shells,
//! monitors, subagents, MCP tasks, loops and goals. Each agent draws these in
//! its own way, so each has its own reader; they share one result type and
//! only read the agent's own chrome near the prompt, so prompt input and
//! transcript text can never impersonate these counts.

use super::Agent;

mod claude;
mod codex;
mod cursor;
mod grok;
mod opencode;

/// How many footer lines below the prompt box are read.
pub(super) const FOOTER_LINES: usize = 12;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackgroundActivity {
    pub shells: u16,
    pub monitors: u16,
    pub agents: u16,
    pub mcp_tasks: u16,
    /// Background work the agent counts without saying what kind (Cursor's
    /// `2 tasks` covers both shells and subagents).
    pub tasks: u16,
    /// A `↻ loop:` line is showing, even one cut off before its wakeup time.
    pub looping: bool,
    /// The time of the next scheduled wakeup, as the agent shows it.
    pub loop_next: Option<String>,
    /// A `/goal` is active.
    pub goal: bool,
    /// The last turn met a `/goal`, and no prompt has followed it.
    pub goal_achieved: bool,
}

impl BackgroundActivity {
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Whether the agent's screen has background work to read.
pub fn has_reader(agent: Agent) -> bool {
    matches!(
        agent,
        Agent::Claude | Agent::Codex | Agent::Cursor | Agent::Grok | Agent::OpenCode
    )
}

/// The agent's background work, or `None` when the screen shows no footer to
/// read (a dialog or a full-screen view), so the last known value should stand.
pub fn background_activity(agent: Option<Agent>, content: &str) -> Option<BackgroundActivity> {
    match agent {
        Some(Agent::Claude) => claude::background_activity(content),
        Some(Agent::Codex) => codex::background_activity(content),
        Some(Agent::Cursor) => cursor::background_activity(content),
        Some(Agent::Grok) => grok::background_activity(content),
        Some(Agent::OpenCode) => opencode::background_activity(content),
        _ => Some(BackgroundActivity::default()),
    }
}

pub(super) fn is_border(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.chars().take(3).count() == 3 && trimmed.starts_with("───")
}

/// Parses `"<n> <noun>"` or `"<n> <noun>s"` exactly.
pub(super) fn counted(segment: &str, noun: &str) -> Option<u16> {
    let (count, rest) = segment.split_once(' ')?;
    let rest = rest.strip_suffix('s').unwrap_or(rest);
    (rest == noun).then(|| count.parse().ok()).flatten()
}
