//! Cursor CLI's background work, read around its input box.
//!
//! Below the input box (`→ Add a follow-up`) Cursor counts what still runs as
//! `1 task` / `2 tasks`, covering background shells and subagents alike, so
//! the count is kept as untyped tasks. A `/goal` shows as a right-aligned
//! `Goal active (2s)` or `Goal paused (6s)` line right above the input box;
//! once met the line goes and `Updated goal complete` is printed in the turn.
//! Cursor draws no prompt marker between turns, so a met goal counts while that
//! line is among the last few above the input box.
//!
//! A menu in the input box's place (an approval or a plan to build) hides the
//! footer, so the last value stands.

use super::BackgroundActivity;

/// How many lines above the input box can hold the goal line.
const GOAL_LINES: usize = 3;
/// How many lines above the input box a met goal is still the last news.
const GOAL_MET_LINES: usize = 4;
/// The goal line is pushed to the right edge; transcript text is not.
const RIGHT_ALIGNED: usize = 20;

pub(super) fn background_activity(content: &str) -> Option<BackgroundActivity> {
    let lines = content.lines().collect::<Vec<_>>();
    let input = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with("→ "))?;
    if is_menu_choice(lines[input]) {
        return None;
    }
    let tasks = lines[input + 1..]
        .iter()
        .take(super::FOOTER_LINES)
        .find_map(|line| super::counted(line.trim(), "task"))
        .unwrap_or(0);
    let above = || {
        lines[..input]
            .iter()
            .rev()
            .filter(|line| !line.trim().is_empty())
    };
    let goal = above().take(GOAL_LINES).any(|line| is_goal_line(line));
    let goal_achieved = !goal
        && above()
            .take(GOAL_MET_LINES)
            .any(|line| line.trim() == "Updated goal complete");
    Some(BackgroundActivity {
        tasks,
        goal,
        goal_achieved,
        ..BackgroundActivity::default()
    })
}

/// `→ Run (once) (y)` or `→ 1. Yes, build locally (b)`: a menu, not the input.
fn is_menu_choice(line: &str) -> bool {
    let rest = line.trim_start().trim_start_matches(['│', ' ']);
    let rest = rest.trim_start_matches("→ ");
    rest.starts_with("Run (once)") || rest.starts_with(|c: char| c.is_ascii_digit())
}

fn is_goal_line(line: &str) -> bool {
    let indent = line.len() - line.trim_start().len();
    let trimmed = line.trim();
    indent >= RIGHT_ALIGNED
        && (trimmed.starts_with("Goal active (") || trimmed.starts_with("Goal paused ("))
        && trimmed.ends_with(')')
}

#[cfg(test)]
mod tests {
    use super::super::background_activity;
    use crate::detect::Agent;

    const FOOTER: [&str; 2] = ["  Codex 5.3 Low · 5%", "  /var/tmp/work"];
    const GOAL: &str = "                                                        Goal active (6s)";

    fn read(above: &[&str], below: &[&str]) -> Option<super::BackgroundActivity> {
        let mut lines = above.to_vec();
        lines.push("  → Add a follow-up                                          ctrl+c to stop");
        lines.extend_from_slice(below);
        lines.extend_from_slice(&FOOTER);
        background_activity(Some(Agent::Cursor), &lines.join("\n"))
    }

    #[test]
    fn counts_tasks_below_the_input_box() {
        // Captured from a live cursor-agent 2026.10.22 pane.
        let one = read(
            &["  $ sleep 60 1.8s (background)", "  started"],
            &["  1 task"],
        )
        .unwrap();
        assert_eq!(one.tasks, 1);
        let two = read(&[" ⠀⠞ Running subagent  1.93k tokens"], &["  2 tasks"]).unwrap();
        assert_eq!(two.tasks, 2);
        assert!(read(&["  started"], &[]).unwrap().is_empty());
    }

    #[test]
    fn a_transcript_line_cannot_impersonate_the_count() {
        assert_eq!(read(&["  3 tasks"], &[]).unwrap().tasks, 0);
    }

    #[test]
    fn reads_an_active_or_paused_goal_and_a_met_one() {
        assert!(read(&["  $ sleep 30 45s", GOAL], &[]).unwrap().goal);
        let paused = "                                                       Goal paused (6s)";
        assert!(read(&[paused], &[]).unwrap().goal);
        assert!(
            !read(&["  Goal active (6s)"], &[]).unwrap().goal,
            "text, not the goal line"
        );

        let met = read(
            &[
                "  $ ls 193ms",
                "    Updated goal complete",
                "  Completed: ran sleep 30, then ran ls, and marked the goal complete.",
            ],
            &[],
        )
        .unwrap();
        assert!(met.goal_achieved && !met.goal);
    }

    #[test]
    fn a_menu_in_place_of_the_input_keeps_the_last_value() {
        let content = [
            " Run this command?",
            "  → Run (once) (y)",
            "    Skip & tell the agent what to do instead (esc or n)",
            GOAL,
        ]
        .join("\n");
        assert_eq!(background_activity(Some(Agent::Cursor), &content), None);
        let plan = " │  → 1. Yes, build locally (b)          │";
        assert_eq!(background_activity(Some(Agent::Cursor), plan), None);
    }
}
