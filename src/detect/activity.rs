//! Background work an agent reports in its footer, below the prompt box.
//!
//! Claude Code lists what keeps running after a turn ends on the line under its
//! prompt box (`⏵⏵ auto mode on · 2 shells · 1 monitor · ← 3 agents`), shows a
//! row per running subagent (`◯ general-purpose  Writing tests  2m 3s`), and a
//! `↻ loop: … · next 19:10` line while a scheduled wakeup is armed. Only text
//! below the prompt box's bottom border is read, so prompt input and transcript
//! text can never impersonate these counts.

use super::Agent;

/// How many footer lines below the prompt box are read.
const FOOTER_LINES: usize = 12;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackgroundActivity {
    pub shells: u16,
    pub monitors: u16,
    pub agents: u16,
    pub mcp_tasks: u16,
    /// The time of the next scheduled wakeup, as the agent shows it.
    pub loop_next: Option<String>,
}

impl BackgroundActivity {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The agent's background work, or `None` when the screen shows no footer to
/// read (a dialog or a full-screen view), so the last known value should stand.
pub fn background_activity(agent: Option<Agent>, content: &str) -> Option<BackgroundActivity> {
    match agent {
        Some(Agent::Claude) => claude_background_activity(content),
        _ => Some(BackgroundActivity::default()),
    }
}

fn claude_background_activity(content: &str) -> Option<BackgroundActivity> {
    let lines = content.lines().collect::<Vec<_>>();
    let prompt = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with('❯'))?;
    let border = prompt
        + 1
        + lines[prompt + 1..]
            .iter()
            .position(|line| is_border(line))?;
    let mut activity = BackgroundActivity::default();
    let mut agent_rows = 0u16;
    for line in lines[border + 1..].iter().take(FOOTER_LINES) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("◯ ") {
            if !rest.trim_start().starts_with("main") {
                agent_rows = agent_rows.saturating_add(1);
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("↻ loop:") {
            activity.loop_next = rest
                .rsplit_once("· next ")
                .map(|(_, next)| next.trim().to_owned())
                .filter(|next| !next.is_empty());
            continue;
        }
        // Only the mode line carries counts; the line above it can be a
        // user-configured statusline that prints anything.
        if !is_mode_line(trimmed) {
            continue;
        }
        for segment in trimmed.split('·').map(str::trim) {
            if let Some(count) = counted(segment, "shell") {
                activity.shells = count;
            } else if let Some(count) = counted(segment, "monitor") {
                activity.monitors = count;
            } else if let Some(count) = segment.strip_prefix("← ").and_then(|s| counted(s, "agent"))
            {
                activity.agents = count;
            } else if let Some(count) = counted(segment, "MCP task") {
                activity.mcp_tasks = count;
            }
        }
    }
    activity.agents = activity.agents.max(agent_rows);
    Some(activity)
}

fn is_mode_line(trimmed: &str) -> bool {
    trimmed.starts_with('⏵') || trimmed.starts_with('⏸') || trimmed.starts_with("? for shortcuts")
}

fn is_border(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.chars().take(3).count() == 3 && trimmed.starts_with("───")
}

/// Parses `"<n> <noun>"` or `"<n> <noun>s"` exactly.
fn counted(segment: &str, noun: &str) -> Option<u16> {
    let (count, rest) = segment.split_once(' ')?;
    let rest = rest.strip_suffix('s').unwrap_or(rest);
    (rest == noun).then(|| count.parse().ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(footer: &[&str]) -> String {
        let mut lines = vec![
            "⏺ Done. The tests pass.",
            "────────────────────────────────────────── title",
            "❯ ",
            "──────────────────────────────────────────────────",
        ];
        lines.extend_from_slice(footer);
        lines.join("\n")
    }

    #[test]
    fn reads_counts_from_the_mode_line() {
        let content = screen(&[
            "  claude-opus-5-5 - context: 57%",
            "  ⏵⏵ bypass permissions on · PR #271 · 2 shells · 1 monitor · ← 1 agent",
        ]);
        let activity = background_activity(Some(Agent::Claude), &content).unwrap();
        assert_eq!(
            activity,
            BackgroundActivity {
                shells: 2,
                monitors: 1,
                agents: 1,
                ..Default::default()
            }
        );
    }

    #[test]
    fn counts_subagent_rows_and_the_loop_wakeup() {
        let content = screen(&[
            "  ↻ loop: keep our PRs moving, fix things that are broken… · next 19:10",
            "  ⏵⏵ auto mode on · 1 monitor",
            "  ● main",
            "  ◯ general-purpose  Writing obs-source-audio-input.h header   7m 20s · ↓ 157.3",
            "  ◯ fork  Research sidebar status   21s · ↓ 230.6",
        ]);
        let activity = background_activity(Some(Agent::Claude), &content).unwrap();
        assert_eq!(activity.monitors, 1);
        assert_eq!(activity.agents, 2);
        assert_eq!(activity.loop_next.as_deref(), Some("19:10"));
    }

    #[test]
    fn prompt_and_transcript_text_cannot_impersonate_counts() {
        let content = [
            "⏺ I started 3 shells · 2 monitors for you.",
            "──────────────────────────────────────────",
            "❯ make it 4 shells · ← 9 agents",
            "──────────────────────────────────────────",
            "  ⏵⏵ auto mode on",
        ]
        .join("\n");
        let activity = background_activity(Some(Agent::Claude), &content).unwrap();
        assert!(activity.is_empty());
    }

    #[test]
    fn custom_statusline_cannot_impersonate_counts() {
        let content = screen(&[
            "  my-statusline · 5 shells · 2 monitors",
            "  ⏵⏵ auto mode on · 1 shell",
        ]);
        let activity = background_activity(Some(Agent::Claude), &content).unwrap();
        assert_eq!((activity.shells, activity.monitors), (1, 0));
    }

    #[test]
    fn no_prompt_box_keeps_the_last_value() {
        assert_eq!(
            background_activity(Some(Agent::Claude), "Do you want to proceed?\n❯ 1. Yes"),
            None
        );
    }

    #[test]
    fn other_agents_report_nothing() {
        let content = screen(&["  ⏵⏵ auto mode on · 2 shells"]);
        assert!(background_activity(Some(Agent::Codex), &content)
            .unwrap()
            .is_empty());
        assert!(background_activity(None, &content).unwrap().is_empty());
    }
}
