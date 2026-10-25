//! OpenCode's background work, read from the footer under its input box.
//!
//! The row under the input box's bottom edge (`╹▀▀▀…`) counts work moved to
//! the background, idle or not: `↓ 1 shell · 10.4K (5%) · $0.00`, or
//! `↓ 2 subagents · …`. OpenCode has no goals, loops or monitors.
//!
//! A permission or question prompt takes the input box's place, which hides
//! the footer, so the last value stands.

use super::BackgroundActivity;

pub(super) fn background_activity(content: &str) -> Option<BackgroundActivity> {
    let lines = content.lines().collect::<Vec<_>>();
    let edge = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with("╹▀"))?;
    let footer = lines[edge + 1..]
        .iter()
        .find(|line| !line.trim().is_empty())?;
    let mut activity = BackgroundActivity::default();
    let Some((_, counts)) = footer.split_once("↓ ") else {
        return Some(activity);
    };
    let counts = counts.split('·').next().unwrap_or_default();
    for segment in counts.split(',').map(str::trim) {
        if let Some(count) = super::counted(segment, "shell") {
            activity.shells = count;
        } else if let Some(count) = super::counted(segment, "terminal") {
            activity.shells = activity.shells.saturating_add(count);
        } else if let Some(count) = super::counted(segment, "subagent") {
            activity.agents = count;
        }
    }
    Some(activity)
}

#[cfg(test)]
mod tests {
    use super::super::background_activity;
    use crate::detect::Agent;

    fn read(footer: &str) -> Option<super::BackgroundActivity> {
        let content = [
            "  ┃  Build · GLM-5.3-Flash Free Default / OpenCode",
            "  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
            footer,
        ]
        .join("\n");
        background_activity(Some(Agent::OpenCode), &content)
    }

    #[test]
    fn counts_background_shells_and_subagents_in_the_footer() {
        // Captured from a live OpenCode 2.1.3 pane.
        let shell = read(
            "  /var/tmp/proj     ↓ 1 shell · 10.4K (5%) · $0.00  ctrl+p commands    /var/tmp/proj",
        )
        .unwrap();
        assert_eq!(shell.shells, 1);
        let subagents = read(
            "   ⬝⬝⬝⬝⬝⬝⬝⬝ esc interrupt     ↓ 2 subagents · 10.7K (5%) · $0.00  ctrl+p commands",
        )
        .unwrap();
        assert_eq!(subagents.agents, 2);
        let both = read("  /var/tmp/proj     ↓ 1 shell, 2 subagents · $0.00").unwrap();
        assert_eq!((both.shells, both.agents), (1, 2));
        assert!(
            read("  /var/tmp/proj     10.4K (5%) · $0.00  ctrl+p commands")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn transcript_text_cannot_impersonate_the_footer() {
        let content = [
            "  ↓ 4 shells · from the transcript",
            "  ┃  Build · GLM-5.3-Flash Free Default / OpenCode",
            "  ╹▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
            "  /var/tmp/proj     10.4K (5%) · $0.00",
        ]
        .join("\n");
        assert!(background_activity(Some(Agent::OpenCode), &content)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_permission_prompt_keeps_the_last_value() {
        let content = [
            "  ┃  $ touch probe.txt",
            "  ┃   Allow once   Always allow   Reject     ctrl+f fullscreen  ⇆ select  enter confirm",
        ]
        .join("\n");
        assert_eq!(background_activity(Some(Agent::OpenCode), &content), None);
    }
}
