const CLAUDE_ACTIVITY_GLYPHS: &str = "·✢✳✶✻✽◐◓◑◒";

pub(crate) fn stripped_terminal_title(title: &str) -> Option<String> {
    let title = crate::platform::terminal_title_for_presentation(title).trim();
    if title.is_empty() {
        return None;
    }

    let mut chars = title.char_indices();
    let (_, first) = chars.next()?;
    let after_first = &title[first.len_utf8()..];
    let recognized =
        matches!(first, '\u{2800}'..='\u{28ff}') || CLAUDE_ACTIVITY_GLYPHS.contains(first);
    let stripped = if recognized
        && (after_first.is_empty() || after_first.chars().next().is_some_and(char::is_whitespace))
    {
        after_first.trim()
    } else {
        title
    };
    // Codex can end its title in the thread's id (` | 01a17614-…`), which
    // Oasis reads but does not show; a thread with no name yet shows its id
    // as the name too.
    use crate::detect::codex_subagents::title_thread_prefix;
    let stripped = match stripped.rsplit_once(" | ") {
        Some((name, tail)) if title_thread_prefix(tail).is_some() => name.trim(),
        _ => stripped,
    };
    if title_thread_prefix(stripped).is_some() {
        return None;
    }

    (!stripped.is_empty()).then(|| stripped.to_string())
}

#[cfg(test)]
mod tests {
    use super::stripped_terminal_title;

    #[test]
    fn hides_the_thread_id_a_codex_title_ends_in() {
        // Captured from a live Codex CLI 0.164.0 title.
        assert_eq!(
            stripped_terminal_title(
                "⠦ Spawn sleep command subagent | 01a17614-4139-7270-880e-5a530..."
            )
            .as_deref(),
            Some("Spawn sleep command subagent")
        );
        assert_eq!(
            stripped_terminal_title(
                "01a17614-4139-7270-880e-5a530ea4286e | 01a17614-4139-7270-880e-5a530..."
            ),
            None,
            "a thread with no name yet"
        );
        assert_eq!(
            stripped_terminal_title("build | main").as_deref(),
            Some("build | main")
        );
    }

    #[test]
    fn strips_one_recognized_leading_activity_glyph() {
        for title in [
            "⠋ task",
            "✳ task",
            "  ⠙   task  ",
            "✢ task",
            "✻ task",
            "◐ task",
            "◓ task",
            "◑ task",
            "◒ task",
        ] {
            assert_eq!(stripped_terminal_title(title).as_deref(), Some("task"));
        }
        assert_eq!(
            stripped_terminal_title("⠋ ⠙ task").as_deref(),
            Some("⠙ task")
        );
    }

    #[test]
    fn preserves_unrecognized_or_unbounded_symbols() {
        for (title, expected) in [
            ("★task", "★task"),
            ("★ production", "★ production"),
            ("✨ task", "✨ task"),
            ("☼ status", "☼ status"),
            ("@ task", "@ task"),
            ("task ⠋ detail", "task ⠋ detail"),
            ("[prod] task", "[prod] task"),
        ] {
            assert_eq!(stripped_terminal_title(title).as_deref(), Some(expected));
        }
    }

    #[test]
    fn preserves_unicode_text_and_elides_empty_results() {
        assert_eq!(
            stripped_terminal_title(" ⠋ 修复🙂标题 ").as_deref(),
            Some("修复🙂标题")
        );
        assert_eq!(stripped_terminal_title("  "), None);
        assert_eq!(stripped_terminal_title("⠋   "), None);
    }

    #[cfg(windows)]
    #[test]
    fn strips_one_windows_elevation_decoration_before_activity_glyph() {
        assert_eq!(
            stripped_terminal_title("Administrator:   ⠋ task").as_deref(),
            Some("task")
        );
        assert_eq!(
            stripped_terminal_title("Administrator: Administrator: task").as_deref(),
            Some("Administrator: task")
        );
        assert_eq!(stripped_terminal_title("Administrator: "), None);
    }
}
