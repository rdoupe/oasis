//! Background work an agent reports in its footer, below the prompt box.
//!
//! Claude Code lists what keeps running after a turn ends on the line under its
//! prompt box (`⏵⏵ auto mode on · 2 shells · 1 monitor · ← 3 agents`), shows a
//! row per running subagent (`◯ general-purpose  Writing tests  2m 3s`), and a
//! `↻ loop: … · next 19:10` line while a scheduled wakeup is armed, and a
//! `◎ goal: …` line while a `/goal` is active (those two come from a custom
//! statusline; stock Claude Code draws neither). Stock Claude marks an active
//! goal with `◎ /goal active (5m)` on the notice row right above the prompt
//! box, which is read too. Stock loops show nothing on screen and are read
//! from the transcript instead (see `claude_loop`). A goal that was just met
//! leaves `✔ Goal achieved (…)` in the last turn, read until the next prompt.
//! Apart from that notice row and that line, only text
//! below the prompt box's bottom border is read, so prompt input and transcript
//! text can never impersonate these counts.

use super::{counted, is_border, BackgroundActivity, FOOTER_LINES};

pub(super) fn background_activity(content: &str) -> Option<BackgroundActivity> {
    let lines = content.lines().collect::<Vec<_>>();
    let prompt = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with('❯'))?;
    let border = prompt
        + 1
        + lines[prompt + 1..]
            .iter()
            .position(|line| is_border(line))?;
    let top = lines[..prompt].iter().rposition(|line| is_border(line));
    // The notice row sits right above the prompt box's top border.
    let goal_notice = top
        .and_then(|top| top.checked_sub(1))
        .is_some_and(|notice| lines[notice].trim_start().starts_with("◎ /goal active"));
    // Only the last turn counts: stop at the prompt that started it.
    let goal_achieved = !goal_notice
        && lines[..top.unwrap_or(0)]
            .iter()
            .rev()
            .map(|line| line.trim_start())
            .take_while(|line| !line.starts_with('❯'))
            .any(|line| line.starts_with("✔ Goal achieved"));
    let mut activity = BackgroundActivity {
        goal: goal_notice,
        goal_achieved,
        ..BackgroundActivity::default()
    };
    let mut agent_rows = 0u16;
    for line in lines[border + 1..].iter().take(FOOTER_LINES) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("◯ ") {
            if !rest.trim_start().starts_with("main") {
                agent_rows = agent_rows.saturating_add(1);
            }
            continue;
        }
        if trimmed.starts_with("◎ goal:") {
            activity.goal = true;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("↻ loop:") {
            activity.looping = true;
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
        // Parts are `·`-separated, and shells and monitors share one part as
        // `1 shell, 1 monitor`.
        for segment in trimmed.split(['·', ',']).map(str::trim) {
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

#[cfg(test)]
mod tests {
    use super::super::background_activity;
    use super::*;
    use crate::detect::Agent;

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
    fn reads_comma_joined_shell_and_monitor_counts() {
        // Captured from a live Claude Code pane with a shell and a monitor running.
        let content = screen(&[
            "  claude-opus-5-5 - read 1649.6M cached · effort: medium · context: 62%",
            "  ⏵⏵ auto mode on · 1 shell, 1 monitor",
            "",
            "  ● main",
            "  ◯ general-purpose  Amending bench rotation in port_comparison   1h 6m 37s",
        ]);
        let activity = background_activity(Some(Agent::Claude), &content).unwrap();
        assert_eq!(
            (activity.shells, activity.monitors, activity.agents),
            (1, 1, 1)
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
        assert!(activity.looping);
    }

    #[test]
    fn a_loop_line_cut_off_before_its_wakeup_still_counts() {
        // Captured from a live Claude Code pane whose loop line ran off the edge.
        let content = screen(&[
            "  ↻ loop: keep our PRs moving, keep our issues moving, fi…",
            "  ⏵⏵ bypass permissions on · 2 shells, 1 monitor",
        ]);
        let activity = background_activity(Some(Agent::Claude), &content).unwrap();
        assert!(activity.looping);
        assert_eq!(activity.loop_next, None);
    }

    #[test]
    fn reads_an_active_goal() {
        // Captured from a live Claude Code pane with a /goal running.
        let content = screen(&[
            "  ◎ goal: finish the plan with clawmux, validate that oasis still performs…",
            "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
            "  ● main",
            "  ◯ fork  Pointing justfile bench recipes at lib   5m 9s · ↓ 513.5k tokens",
        ]);
        let activity = background_activity(Some(Agent::Claude), &content).unwrap();
        assert!(activity.goal);
        assert_eq!(activity.agents, 1);
    }

    #[test]
    fn reads_stock_claude_goal_notice() {
        // Captured from a stock Claude Code 2.1.309 pane with no statusline.
        let content = [
            "✻ Puzzling… (20s · ↓ 584 tokens)",
            "                                ◎ /goal active (19s) · Update available! Run: mise upgrade claude",
            "──────────────────────────────────────────",
            "❯ ",
            "──────────────────────────────────────────",
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt",
        ]
        .join("\n");
        let activity = background_activity(Some(Agent::Claude), &content).unwrap();
        assert!(activity.goal);
    }

    #[test]
    fn reads_a_goal_met_in_the_last_turn_only() {
        // Captured from a live Claude Code pane right after a /goal was met.
        let met = |turn: &[&str]| {
            let mut lines = vec!["❯ /goal print 2+2", "  ⎿  Goal set: print 2+2"];
            lines.extend_from_slice(turn);
            lines.extend_from_slice(&[
                "──────────────────────────────────────────",
                "❯ ",
                "──────────────────────────────────────────",
                "  ⏵⏵ bypass permissions on (shift+tab to cycle)",
            ]);
            background_activity(Some(Agent::Claude), &lines.join("\n"))
                .unwrap()
                .goal_achieved
        };
        assert!(met(&[
            "● 4",
            "✔ Goal achieved (4s · 1 turn · 100 tokens)",
            "✻ Sautéed for 4s · done 9:24 AM",
        ]));
        assert!(!met(&[
            "✔ Goal achieved (4s · 1 turn · 100 tokens)",
            "❯ thanks",
            "● You're welcome.",
        ]));
        assert!(!met(&["● 4"]));
    }

    #[test]
    fn goal_notice_must_be_the_row_above_the_prompt_box() {
        let content = [
            "◎ /goal active (19s)",
            "⏺ Done.",
            "──────────────────────────────────────────",
            "❯ ",
            "──────────────────────────────────────────",
            "  ⏵⏵ bypass permissions on",
        ]
        .join("\n");
        let activity = background_activity(Some(Agent::Claude), &content).unwrap();
        assert!(!activity.goal);
    }

    #[test]
    fn prompt_and_transcript_text_cannot_impersonate_counts() {
        let content = [
            "⏺ I started 3 shells · 2 monitors for you.",
            "──────────────────────────────────────────",
            "❯ make it 4 shells · ← 9 agents",
            "◎ goal: pretend",
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
    fn agents_without_a_reader_report_nothing() {
        let content = screen(&["  ⏵⏵ auto mode on · 2 shells"]);
        assert!(background_activity(Some(Agent::Gemini), &content)
            .unwrap()
            .is_empty());
        assert!(background_activity(None, &content).unwrap().is_empty());
    }
}
