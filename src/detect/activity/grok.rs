//! Grok CLI's background work, read from right above its input box and from
//! its header.
//!
//! Between turns Grok counts what still runs on the line right above the
//! input box: `◎ 1 command · 2 monitors · 1 loop · 1 subagent still running`.
//! While a turn works that line gives way to the activity line
//! (`⠹ Responding… 3.8s   4.0s ⇣119 [stop]`), so the last counts stand. Some
//! setups list the work in collapsible groups there instead:
//! `▾ Tasks 1` (background shells), `▾ Subagents 2`, and `▾ Watchers 1`, whose
//! rows are monitors (`⸬ Monitor ticks  25s`) and loops
//! (`◆ Loop <prompt> every 1 minute (next in 22s)`). A `/goal` shows in the
//! header on the top line, `[⁙ Goal: Executing]` while it runs and
//! `[Goal: Done]` once met. The header keeps `Done` through later turns, so a
//! met goal only counts while the last turn, back to your last `❯` message,
//! reports `Goal complete`.
//!
//! An approval replaces the input box, which hides the panel, so the last
//! value stands.

use super::BackgroundActivity;

/// How many lines at the top can hold the header.
const HEADER_LINES: usize = 3;
/// The panel is indented less than transcript lines.
const PANEL_INDENT: usize = 3;
/// How many lines above the input box can hold the still-running line.
const STATUS_LINES: usize = 3;
/// Conversation text is indented this far; Grok's own lines sit less.
const TRANSCRIPT_INDENT: usize = 5;
/// How far above the bottom the input box sits (its border, prompt, border
/// and the hint line under it).
const INPUT_BOX_LINES: usize = 6;

pub(super) fn background_activity(
    content: &str,
    before: &BackgroundActivity,
) -> Option<BackgroundActivity> {
    let lines = content
        .lines()
        .map(|line| line.trim_end_matches([' ', '█']))
        .collect::<Vec<_>>();
    let last = lines.iter().rposition(|line| !line.trim().is_empty())?;
    let input = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with('╭'))
        .filter(|top| last - top <= INPUT_BOX_LINES)
        .filter(|top| {
            lines
                .get(top + 1)
                .is_some_and(|line| line.trim_start().starts_with("│ ❯"))
        })?;
    let mut activity = BackgroundActivity::default();
    let mut panel = false;
    let mut watchers = 0;
    let mut loops = 0;
    for line in lines[..input].iter().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if line.len() - line.trim_start().len() > PANEL_INDENT {
            break;
        }
        if let Some(group) = trimmed.strip_prefix("▾ ") {
            let Some((name, count)) = group.rsplit_once(' ') else {
                break;
            };
            let Ok(count) = count.parse::<u16>() else {
                break;
            };
            panel = true;
            match name {
                "Tasks" => activity.shells = count,
                "Subagents" => activity.agents = count,
                "Watchers" => watchers = count,
                _ => {}
            }
            continue;
        }
        // A row: a one-cell glyph, then what it is.
        let Some((_, row)) = trimmed.split_once(' ') else {
            break;
        };
        if !trimmed.starts_with(|c: char| !c.is_alphanumeric()) {
            break;
        }
        if row.starts_with("Loop ") {
            loops += 1;
        }
    }
    activity.looping = loops > 0;
    activity.monitors = watchers.saturating_sub(loops);
    // A notice (`Run /doctor for details and fixes.`) can sit between that
    // line and the input box; conversation text sits further in.
    let mut above = lines[..input]
        .iter()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(STATUS_LINES)
        .filter(|line| line.len() - line.trim_start().len() < TRANSCRIPT_INDENT)
        .map(|line| line.trim());
    let status = above.find(|line| still_running(line).is_some() || line.ends_with("[stop]"));
    if let (false, Some(status)) = (panel, status) {
        if let Some(counts) = still_running(status) {
            for segment in counts.split('·').map(str::trim) {
                if let Some(count) = super::counted(segment, "command") {
                    activity.shells = count;
                } else if let Some(count) = super::counted(segment, "monitor") {
                    activity.monitors = count;
                } else if let Some(count) = super::counted(segment, "loop") {
                    activity.looping = count > 0;
                } else if let Some(count) = super::counted(segment, "subagent") {
                    activity.agents = count;
                }
            }
        } else {
            activity.shells = before.shells;
            activity.monitors = before.monitors;
            activity.agents = before.agents;
            activity.looping = before.looping;
        }
    }
    match header_goal(&lines) {
        Some(phase) if phase.starts_with("Done") => {
            activity.goal_achieved = last_turn_met_the_goal(&lines[..input]);
        }
        Some(_) => activity.goal = true,
        None => {}
    }
    Some(activity)
}

/// The counts in `◎ 1 command · 1 loop still running`, after its one-cell
/// glyph, which animates. A wait adds `· send a message to interrupt`.
fn still_running(line: &str) -> Option<&str> {
    let (glyph, rest) = line.split_once(' ')?;
    if glyph.chars().count() != 1 || glyph.starts_with(|c: char| c.is_alphanumeric()) {
        return None;
    }
    rest.split_once(" still running").map(|(counts, _)| counts)
}

/// The phase in the header's `[⁙ Goal: Executing]` or `[Goal: Done]`.
fn header_goal<'a>(lines: &[&'a str]) -> Option<&'a str> {
    lines.iter().take(HEADER_LINES).find_map(|line| {
        let (_, rest) = line.split_once('[')?;
        let rest = rest.split_once(']')?.0;
        let rest = rest.trim_start_matches(|c: char| c != 'G');
        rest.strip_prefix("Goal: ")
    })
}

fn last_turn_met_the_goal(transcript: &[&str]) -> bool {
    transcript
        .iter()
        .rev()
        .map(|line| line.trim_start())
        .take_while(|line| !line.starts_with("❯ "))
        .any(|line| line.starts_with("Goal complete"))
}

#[cfg(test)]
mod tests {
    use super::super::background_activity;
    use crate::detect::Agent;

    const HEADER: &str =
        "  /v/tmp/work                                     24K / 256K │ [Dashboard]";
    const INPUT: [&str; 4] = [
        "  ╭──────────────────────────────────────────────╮",
        "  │ ❯                                            │",
        "  ╰─────────────────────────── Grok 4.5 (low) ─╯",
        "  Shift+Tab:mode  │  Ctrl+x:shortcuts",
    ];

    fn read(header: &str, body: &[&str]) -> Option<super::BackgroundActivity> {
        let mut lines = vec![header];
        lines.extend_from_slice(body);
        lines.extend_from_slice(&INPUT);
        background_activity(Some(Agent::Grok), &lines.join("\n"))
    }

    #[test]
    fn reads_the_still_running_line_and_keeps_it_while_a_turn_works() {
        // Captured from a live grok 1.0.70 pane.
        let idle = read(
            HEADER,
            &[
                "     Worked for 0.4s",
                "    ○ 2 commands · 1 monitor · 1 loop · 1 subagent still running",
                "   Run /doctor for details and fixes.",
            ],
        )
        .unwrap();
        assert_eq!((idle.shells, idle.monitors, idle.agents), (2, 1, 1));
        assert!(idle.looping);
        let waiting = read(
            HEADER,
            &["    ◎ 1 command still running · send a message to interrupt"],
        )
        .unwrap();
        assert_eq!(waiting.shells, 1);

        let working = [
            HEADER,
            "     word0 word1 word2                                                    1:40 PM",
            "    ⠹ Responding… 3.8s                                      4.0s ⇣119 [stop]",
        ]
        .into_iter()
        .chain(INPUT)
        .collect::<Vec<_>>()
        .join("\n");
        let kept =
            super::super::background_activity_after(Some(Agent::Grok), &working, &idle).unwrap();
        assert_eq!(kept, idle);
        assert!(read(HEADER, &["     Worked for 0.4s"]).unwrap().is_empty());
    }

    #[test]
    fn transcript_text_cannot_impersonate_the_still_running_line() {
        let activity = read(
            HEADER,
            &[
                "     I said 3 commands still running",
                "     Worked for 0.4s",
            ],
        )
        .unwrap();
        assert!(activity.is_empty());
    }

    #[test]
    fn reads_tasks_subagents_monitors_and_loops_from_the_panel() {
        // Captured from a live grok 1.0.70 pane.
        let tasks = read(
            HEADER,
            &[
                "   ▾ Tasks 1",
                "   ⁙ Run Background sleep for 90 seconds                         15s",
            ],
        )
        .unwrap();
        assert_eq!(tasks.shells, 1);
        let subagents = read(
            HEADER,
            &[
                "   ▾ Subagents 2",
                "   ⸬ Subagent sleep 40 then done · Sleep for 40 seconds…    grok-4.5 12s",
                "   ⸬ Subagent sleep 40 then done · Sleep for 40 seconds…    grok-4.5 12s",
            ],
        )
        .unwrap();
        assert_eq!(subagents.agents, 2);
        let monitor = read(
            HEADER,
            &["   ▾ Watchers 1", "   ⸬ Monitor ticks          25s"],
        )
        .unwrap();
        assert_eq!((monitor.monitors, monitor.looping), (1, false));
        let looping = read(
            HEADER,
            &[
                "   ▾ Watchers 1",
                "   ◆ Loop Run echo loop-fire… every 1 minute (next in 22s)",
            ],
        )
        .unwrap();
        assert_eq!((looping.monitors, looping.looping), (0, true));
        assert!(read(HEADER, &["     Worked for 4.3s"]).unwrap().is_empty());
    }

    #[test]
    fn transcript_lines_cannot_impersonate_the_panel() {
        let activity = read(
            HEADER,
            &["     ▾ Tasks 4", "  Read Terms and Privacy Policy."],
        )
        .unwrap();
        assert!(activity.is_empty());
    }

    #[test]
    fn reads_a_running_goal_and_a_goal_met_in_the_last_turn_only() {
        let running = "  /v/tmp/work         [⁙ Goal: Executing]  26.1k tokens  20s │ [Dashboard]";
        assert!(read(running, &[]).unwrap().goal);

        let done = "  /v/tmp/work                [Goal: Done]  91.3k tokens  1m │ [Dashboard]";
        let met = read(
            done,
            &[
                "     ❯ /goal Run the shell command 'echo goal-ok' once.       11:22 AM",
                "     Goal complete in 1m30s end-to-end.",
                "     Worked for 1m41s",
            ],
        )
        .unwrap();
        assert!(met.goal_achieved && !met.goal);
        let later = read(
            done,
            &[
                "     Goal complete in 1m30s end-to-end.",
                "     ❯ /loop 1m Run the shell command 'echo loop-fire'.      11:24 AM",
                "     Worked for 8.0s",
            ],
        )
        .unwrap();
        assert!(!later.goal_achieved, "the header outlives the goal's turn");
    }

    #[test]
    fn an_approval_in_place_of_the_input_keeps_the_last_value() {
        let content = [
            HEADER,
            "  ┃  ›  Write hi to probe2.txt",
            "  ┃  2 (●) Yes, proceed",
            "  1/4:select  │  Tab:next option  │  Ctrl+c:cancel",
        ]
        .join("\n");
        assert_eq!(background_activity(Some(Agent::Grok), &content), None);
    }
}
