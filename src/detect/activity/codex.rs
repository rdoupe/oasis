//! Codex CLI's background work, read around its prompt box.
//!
//! Codex counts its background terminals on the line right above the prompt
//! box: appended to the activity line while it works
//! (`• Working (5s • esc to interrupt) · 1 background terminal running · …`)
//! and on a line of its own between turns
//! (`1 background terminal running · /ps to view · /stop to close`). A
//! `/goal` shows in the status line's right-hand slot, `Pursuing goal (7s)`
//! while it runs and `Goal achieved (12s)` once met. That slot keeps the
//! achieved notice after later turns, so a met goal only counts while the last
//! turn is the one the goal drove (it shows `• Goal active …`).
//!
//! Codex has no loops or monitors, and shows subagents only as transcript
//! lines, so neither is read. A menu in place of the prompt box (an approval,
//! a plan or hook review) hides the footer, so the last value stands.

use super::BackgroundActivity;

/// How many lines above the prompt box can hold the terminal count.
const ACTIVITY_LINES: usize = 3;

pub(super) fn background_activity(content: &str) -> Option<BackgroundActivity> {
    let lines = content.lines().collect::<Vec<_>>();
    let prompt = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with('›'))?;
    if is_menu_choice(lines[prompt]) {
        return None;
    }
    let mut activity = BackgroundActivity::default();
    for line in lines[prompt + 1..].iter().take(super::FOOTER_LINES) {
        match right_slot(line) {
            Some(slot) if slot.starts_with("Pursuing goal (") => activity.goal = true,
            Some(slot) if slot.starts_with("Goal achieved (") => {
                activity.goal_achieved = last_turn_was_driven_by_a_goal(&lines[..prompt]);
            }
            _ => {}
        }
    }
    activity.shells = lines[..prompt]
        .iter()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(ACTIVITY_LINES)
        .find_map(|line| background_terminals(line))
        .unwrap_or(0);
    Some(activity)
}

/// `› 1. Yes, proceed (y)`: a menu has taken the prompt box's place.
fn is_menu_choice(line: &str) -> bool {
    let rest = line.trim_start().trim_start_matches('›').trim_start();
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && rest[digits..].starts_with('.')
}

/// The status line's right-aligned slot, set apart by a run of spaces. The
/// left side holds the thread title, which can say anything.
fn right_slot(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let (_, slot) = trimmed.rsplit_once("   ")?;
    Some(slot.trim())
}

/// Codex's own activity line or terminal line; a quoted prompt (`› …`) or
/// transcript entry naming terminals does not count.
fn background_terminals(line: &str) -> Option<u16> {
    let trimmed = line.trim();
    if !(trimmed.starts_with("• Working (") || trimmed.starts_with(|c: char| c.is_ascii_digit()))
    {
        return None;
    }
    trimmed.split('·').map(str::trim).find_map(|segment| {
        super::counted(segment.strip_suffix(" running")?, "background terminal")
    })
}

/// Whether the turn since the last prompt shows the goal that drove it.
fn last_turn_was_driven_by_a_goal(transcript: &[&str]) -> bool {
    transcript
        .iter()
        .rev()
        .map(|line| line.trim_start())
        .take_while(|line| !line.starts_with('›'))
        .any(|line| line.starts_with("• Goal active"))
}

#[cfg(test)]
mod tests {
    use super::super::background_activity;
    use crate::detect::Agent;

    const STATUS: &str = "  GPT-5.6-Luna low · /var/tmp/work · Run foreground sleep command";
    const HINT: &str =
        "  ? for shortcuts                                                  ⚠ 1 warning · f2 to view";

    fn read(lines: &[&str]) -> super::BackgroundActivity {
        background_activity(Some(Agent::Codex), &lines.join("\n")).unwrap()
    }

    #[test]
    fn counts_background_terminals_while_working_and_between_turns() {
        // Captured from a live Codex CLI 0.164.0 pane.
        let working = read(&[
            "• Working (5s • esc to interrupt) · 1 background terminal running · /ps to view · /stop to close",
            "",
            "› Ask Codex to do anything",
            STATUS,
            HINT,
        ]);
        assert_eq!(working.shells, 1);
        let between = read(&[
            "• started",
            "  Worked for 3s • 11:14 AM",
            "  2 background terminals running · /ps to view · /stop to close",
            "",
            "› Ask Codex to do anything",
            STATUS,
            HINT,
        ]);
        assert_eq!(between.shells, 2);
        let ended = read(&[
            "  Worked for 3s • 11:14 AM",
            "",
            "› Ask Codex to do anything",
            STATUS,
            HINT,
        ]);
        assert!(ended.is_empty());
    }

    #[test]
    fn a_prompt_naming_terminals_does_not_count() {
        let activity = read(&[
            "› Start 3 background terminal running please",
            "• ok",
            "",
            "› Ask Codex to do anything",
            STATUS,
            HINT,
        ]);
        assert_eq!(activity.shells, 0);
    }

    #[test]
    fn reads_a_running_goal_and_a_goal_met_in_the_last_turn_only() {
        let pursuing = format!("{STATUS}                             Pursuing goal (7s)");
        assert!(
            read(&[
                "• Goal active Objective: wait",
                "› Ask Codex to do anything",
                &pursuing,
                HINT
            ])
            .goal
        );

        let achieved = format!("{STATUS}                            Goal achieved (12s)");
        let met = read(&[
            "› Call update_plan with steps",
            "• ok",
            "• Goal active Objective: Run `sleep 10` once, then mark this goal complete.",
            "• done",
            "  Worked for 13s • 11:16 AM",
            "› Ask Codex to do anything",
            &achieved,
            HINT,
        ]);
        assert!(met.goal_achieved && !met.goal);
        let later = read(&[
            "• Goal active Objective: Run `sleep 10` once, then mark this goal complete.",
            "› Spawn exactly one subagent",
            "• ok",
            "› Ask Codex to do anything",
            &achieved,
            HINT,
        ]);
        assert!(!later.goal_achieved, "the slot outlives the goal's turn");
    }

    #[test]
    fn a_thread_title_cannot_impersonate_the_goal_slot() {
        let titled = "  GPT-5.6-Luna low · /var/tmp/work · Pursuing goal (7s)";
        assert!(!read(&["› Ask Codex to do anything", titled, HINT]).goal);
    }

    #[test]
    fn a_menu_in_place_of_the_prompt_keeps_the_last_value() {
        let content = [
            "  Implement this plan?",
            "› 1. Yes, implement this plan          Switch to Default and start coding",
            "  2. Yes, clear context and implement  Start a fresh thread",
            "  enter select · esc back",
        ]
        .join("\n");
        assert_eq!(background_activity(Some(Agent::Codex), &content), None);
    }
}
