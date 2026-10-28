//! Finding the link a person tapped in a pane's text.
//!
//! Front ends hand over the pane's rows as text, each char with the column it
//! starts at, and get back the link written across the tapped cell. Any
//! `scheme://` link counts (`https://`, `chrome://settings`, `vscode://…`), plus
//! `mailto:` and `about:`. A link that runs to the pane's right edge is read on
//! into the next row, so a long URL that wrapped, by the terminal or by an app
//! that wraps its own output with an indent, opens whole.

/// One row of pane text: each char with the column it starts at, left to right.
pub type Row = Vec<(char, u16)>;

/// How many rows a wrapped link is followed across, each way from the tap.
const MAX_WRAPPED_ROWS: usize = 8;

/// How far short of the right edge a row may stop and still wrap onto the next:
/// apps that wrap their own output (Claude Code does) leave a column or two free.
const EDGE_SLACK: u16 = 2;

/// The link written in `rows` across the cell at `row`, `col`. `right` is the
/// pane's last text column.
pub fn link_in_text(rows: &[Row], right: u16, row: usize, col: u16) -> Option<String> {
    link_cells_in_text(rows, right, row, col).map(|(url, _)| url)
}

/// Like [`link_in_text`], with the (row, column) each char of the link starts
/// at, for underlining it across the rows it wraps over.
pub fn link_cells_in_text(
    rows: &[Row],
    right: u16,
    row: usize,
    col: u16,
) -> Option<(String, Vec<(usize, u16)>)> {
    rows.get(row)?;
    let wraps = |r: usize| {
        let reaches_edge = rows[r]
            .iter()
            .rev()
            .find(|(ch, _)| !ch.is_whitespace())
            .is_some_and(|(_, c)| c + EDGE_SLACK >= right);
        reaches_edge
            && rows
                .get(r + 1)
                .is_some_and(|next| next.iter().any(|(ch, _)| !ch.is_whitespace()))
    };
    let mut first = row;
    while first > 0 && row - first < MAX_WRAPPED_ROWS && wraps(first - 1) {
        first -= 1;
    }
    let mut last = row;
    while last - row < MAX_WRAPPED_ROWS && wraps(last) {
        last += 1;
    }
    // The rows as one run of text, each char with its (row, column); a row that
    // continues the one before drops its indent, and the joins carry no space.
    let mut chars: Vec<(char, (usize, u16))> = Vec::new();
    for (r, row_text) in rows.iter().enumerate().take(last + 1).skip(first) {
        let mut text = row_text.iter().peekable();
        if r > first {
            while text.next_if(|(ch, _)| ch.is_whitespace()).is_some() {}
        }
        let end = chars.len();
        chars.extend(text.map(|&(ch, c)| (ch, (r, c))));
        if r < last {
            // Blanks after the last char of a wrapping row are its unused edge.
            while chars.len() > end && chars.last().is_some_and(|(ch, _)| ch.is_whitespace()) {
                chars.pop();
            }
        }
    }
    let tapped = chars.iter().rposition(|(_, at)| *at <= (row, col))?;
    let mut at = 0;
    while at < chars.len() {
        let Some(body) = scheme_end(&chars, at) else {
            at += 1;
            continue;
        };
        let mut end = body;
        while end < chars.len() && url_char(chars[end].0) {
            end += 1;
        }
        let url = trim_url(&chars[at..end].iter().map(|(ch, _)| ch).collect::<String>());
        let len = url.chars().count();
        if len > body - at && (at..at + len).contains(&tapped) {
            let cells = chars[at..at + len].iter().map(|(_, at)| *at).collect();
            return Some((url, cells));
        }
        at = end.max(at + 1);
    }
    None
}

/// Whether an OSC 8 hyperlink target is one to open: it names a scheme, and not
/// one that runs code.
pub fn openable(target: &str) -> bool {
    let Some((scheme, rest)) = target.split_once(':') else {
        return false;
    };
    // A one-letter "scheme" is a Windows drive (`C:\…`), not a link.
    scheme.len() >= 2
        && valid_scheme(scheme)
        && !rest.is_empty()
        && !["javascript", "data", "vbscript"]
            .iter()
            .any(|blocked| scheme.eq_ignore_ascii_case(blocked))
}

/// Where the body of a link starting at `at` begins, after its `scheme://`,
/// `mailto:` or `about:`; none when no link starts there.
fn scheme_end(chars: &[(char, (usize, u16))], at: usize) -> Option<usize> {
    let before = at.checked_sub(1).map(|i| chars[i].0);
    scheme_len(before, chars[at..].iter().map(|(ch, _)| *ch)).map(|len| at + len)
}

/// The length in chars of the `scheme://`, `mailto:` or `about:` that `text`
/// starts with, when a link starts there; `before` is the char ahead of it.
pub fn scheme_len(before: Option<char>, text: impl IntoIterator<Item = char>) -> Option<usize> {
    if before.is_some_and(|ch| ch.is_ascii_alphanumeric()) {
        return None;
    }
    let mut text = text.into_iter().peekable();
    let mut scheme = String::new();
    while let Some(ch) = text.next_if(|&ch| is_scheme_char(ch, scheme.is_empty())) {
        scheme.push(ch.to_ascii_lowercase());
    }
    text.next_if_eq(&':')?;
    let slashes = text.next_if_eq(&'/').is_some() && text.next_if_eq(&'/').is_some();
    if slashes && scheme.len() >= 2 {
        Some(scheme.len() + 3)
    } else if scheme == "mailto" || scheme == "about" {
        Some(scheme.len() + 1)
    } else {
        None
    }
}

fn is_scheme_char(ch: char, first: bool) -> bool {
    ch.is_ascii_alphabetic() || (!first && (ch.is_ascii_digit() || matches!(ch, '+' | '.' | '-')))
}

fn valid_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    chars.next().is_some_and(|ch| is_scheme_char(ch, true))
        && chars.all(|ch| is_scheme_char(ch, false))
}

fn url_char(ch: char) -> bool {
    !ch.is_whitespace() && !ch.is_control() && !matches!(ch, '"' | '\'' | '<' | '>' | '`' | '│')
}

/// Drops sentence punctuation after a URL and a closing bracket it never opened.
fn trim_url(url: &str) -> String {
    let mut url = url.to_string();
    while let Some(last) = url.chars().last() {
        let unbalanced = |open: char, close: char| {
            last == close && url.matches(close).count() > url.matches(open).count()
        };
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?')
            || unbalanced('(', ')')
            || unbalanced('[', ']')
            || unbalanced('{', '}')
        {
            url.pop();
        } else {
            break;
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(lines: &[&str]) -> Vec<Row> {
        lines
            .iter()
            .map(|line| line.chars().zip(0u16..).collect())
            .collect()
    }

    fn link(lines: &[&str], right: u16, row: usize, col: u16) -> Option<String> {
        link_in_text(&rows(lines), right, row, col)
    }

    #[test]
    fn a_plain_url_under_the_tap_is_found() {
        let line = "see https://example.com/a?b=1 for more";
        assert_eq!(
            link(&[line], 60, 0, 10).as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(link(&[line], 60, 0, 1), None);
        assert_eq!(link(&[line], 60, 0, 34), None);
    }

    #[test]
    fn any_scheme_with_slashes_is_a_link() {
        let line = "open chrome://settings/performance, then vscode://file/x";
        assert_eq!(
            link(&[line], 80, 0, 8).as_deref(),
            Some("chrome://settings/performance")
        );
        assert_eq!(link(&[line], 80, 0, 50).as_deref(), Some("vscode://file/x"));
        assert_eq!(
            link(&["mail mailto:a@b.example now"], 80, 0, 8).as_deref(),
            Some("mailto:a@b.example")
        );
        assert_eq!(
            link(&["at about:blank"], 80, 0, 5).as_deref(),
            Some("about:blank")
        );
    }

    #[test]
    fn words_that_only_look_like_schemes_are_not_links() {
        assert_eq!(
            link(&["note: see x://y"], 80, 0, 2),
            None,
            "a one-letter scheme"
        );
        assert_eq!(
            link(&["ratio 3://4"], 80, 0, 7),
            None,
            "a scheme must start with a letter"
        );
        assert_eq!(
            link(&["https://"], 80, 0, 2),
            None,
            "a scheme with nothing after it"
        );
    }

    #[test]
    fn trailing_punctuation_and_unopened_brackets_are_left_off() {
        assert_eq!(
            link(&["go to https://example.com."], 80, 0, 10).as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            link(&["(http://example.com/x), ok"], 80, 0, 5).as_deref(),
            Some("http://example.com/x")
        );
        assert_eq!(
            link(&["https://en.wikipedia.org/wiki/Rust_(language)"], 80, 0, 3).as_deref(),
            Some("https://en.wikipedia.org/wiki/Rust_(language)")
        );
    }

    #[test]
    fn a_url_wrapped_by_the_terminal_opens_whole_from_either_row() {
        let lines = ["see https://example.com/a-very-lo", "ng/path?q=1 and more"];
        let whole = Some("https://example.com/a-very-long/path?q=1");
        assert_eq!(link(&lines, 32, 0, 10).as_deref(), whole);
        assert_eq!(link(&lines, 32, 1, 3).as_deref(), whole);
        assert_eq!(link(&lines, 32, 1, 15), None, "a tap on the words after it");
    }

    #[test]
    fn a_url_an_app_wrapped_with_an_indent_opens_whole() {
        // Claude Code wraps a long URL itself, indenting each continuation row and
        // stopping a column short of the edge.
        let lines = [
            "  https://claude.ai/oauth/authorize?client_id=9d1c&code=true&cha",
            "  llenge_method=S256&redirect_uri=http%3A%2F%2Flocalhost%2Fcb&sc",
            "  ope=user",
            "",
            "  Paste the code here.",
        ];
        let whole = "https://claude.ai/oauth/authorize?client_id=9d1c&code=true&challenge_method=S256&redirect_uri=http%3A%2F%2Flocalhost%2Fcb&scope=user";
        assert_eq!(link(&lines, 64, 0, 20).as_deref(), Some(whole));
        assert_eq!(link(&lines, 64, 1, 30).as_deref(), Some(whole));
        assert_eq!(link(&lines, 64, 2, 4).as_deref(), Some(whole));
        assert_eq!(link(&lines, 64, 4, 4), None);
    }

    #[test]
    fn a_url_short_of_the_edge_does_not_take_the_next_row() {
        let lines = ["see https://example.com/x", "next line"];
        assert_eq!(
            link(&lines, 60, 0, 8).as_deref(),
            Some("https://example.com/x")
        );
        assert_eq!(link(&lines, 60, 1, 2), None);
    }

    #[test]
    fn wide_glyphs_keep_their_columns() {
        let row: Row = vec![('日', 0), ('本', 2), (' ', 4)]
            .into_iter()
            .chain("https://x.example".chars().zip(5u16..))
            .collect();
        assert_eq!(
            link_in_text(&[row], 60, 0, 8).as_deref(),
            Some("https://x.example")
        );
    }

    #[test]
    fn hyperlink_targets_that_run_code_are_not_opened() {
        assert!(openable("https://example.com"));
        assert!(openable("chrome://settings"));
        assert!(openable("mailto:a@b.example"));
        assert!(!openable("javascript:alert(1)"));
        assert!(!openable("DATA:text/html,x"));
        assert!(!openable("no scheme here"));
        assert!(!openable("https:"));
        assert!(!openable("C:\\Windows"), "a drive letter");
    }

    #[test]
    fn a_scheme_is_measured_where_a_link_starts() {
        assert_eq!(scheme_len(None, "chrome://x".chars()), Some(9));
        assert_eq!(scheme_len(Some('('), "https://x".chars()), Some(8));
        assert_eq!(scheme_len(Some(' '), "mailto:a@b".chars()), Some(7));
        assert_eq!(
            scheme_len(Some('x'), "https://x".chars()),
            None,
            "inside a word"
        );
        assert_eq!(scheme_len(None, "note: x".chars()), None);
        assert_eq!(scheme_len(None, "x://y".chars()), None);
    }

    #[test]
    fn a_wrapped_link_reports_where_each_char_sits() {
        let lines = ["see https://ex", "  ample.com"];
        let (url, cells) = link_cells_in_text(&rows(&lines), 14, 1, 4).unwrap();
        assert_eq!(url, "https://example.com");
        assert_eq!(cells.first(), Some(&(0, 4)));
        assert_eq!(cells[10], (1, 2));
        assert_eq!(cells.last(), Some(&(1, 10)));
    }
}
