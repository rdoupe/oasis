//! The pane-area cell grid the server renders, kept in sync from the core's full
//! surfaces and coalesced patches.

use oasis_core::embed::links;
use oasis_core::embed::wire::{
    CellData, CursorState, PaneSurfaceFrame, PaneSurfacePane, PaneSurfacePatchRow,
};

#[derive(Default)]
pub struct Grid {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<CellData>,
    pub cursor: Option<CursorState>,
    pub panes: Vec<PaneSurfacePane>,
    pub hyperlinks: Vec<String>,
}

impl Grid {
    pub fn set_full(&mut self, surface: PaneSurfaceFrame) {
        let frame = surface.frame;
        self.width = frame.width;
        self.height = frame.height;
        self.cells = frame.cells;
        self.cursor = frame.cursor;
        self.hyperlinks = frame.hyperlinks;
        self.panes = surface.panes;
    }

    /// Applies the core's coalesced patch: full-width rows already merged in order.
    pub fn apply_rows(
        &mut self,
        rows: Vec<PaneSurfacePatchRow>,
        panes: Vec<PaneSurfacePane>,
        cursor: Option<CursorState>,
    ) {
        let width = usize::from(self.width);
        for row in rows {
            let start = usize::from(row.y) * width + usize::from(row.x);
            let end = start + row.cells.len();
            if row.y >= self.height || end > self.cells.len() {
                // Out of shape for this grid; the core follows with a full surface.
                continue;
            }
            self.cells[start..end].clone_from_slice(&row.cells);
        }
        for updated in panes {
            if let Some(existing) = self.panes.iter_mut().find(|p| p.pane_id == updated.pane_id) {
                *existing = updated;
            }
        }
        self.cursor = cursor;
    }

    /// Forgets the surface, e.g. when switching machines, until the next full one arrives.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn cell(&self, col: u16, row: u16) -> Option<&CellData> {
        if col >= self.width || row >= self.height {
            return None;
        }
        self.cells
            .get(usize::from(row) * usize::from(self.width) + usize::from(col))
    }

    pub fn pane_at(&self, col: u16, row: u16) -> Option<&PaneSurfacePane> {
        self.panes.iter().find(|pane| {
            let r = pane.rect;
            col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
        })
    }

    pub fn focused_pane(&self) -> Option<&PaneSurfacePane> {
        self.panes.iter().find(|pane| pane.focused)
    }

    /// The first and last columns of the pane under a cell, or the whole width.
    pub fn pane_columns(&self, col: u16, row: u16) -> (u16, u16) {
        self.pane_at(col, row)
            .map(|pane| {
                let r = pane.inner_rect;
                (r.x, (r.x + r.width).saturating_sub(1))
            })
            .unwrap_or((0, self.width.saturating_sub(1)))
    }

    /// Plain text of a cell range within inclusive column bounds, trimming trailing
    /// blanks on each row, for copy.
    pub fn text(&self, from: (u16, u16), to: (u16, u16), cols: (u16, u16)) -> String {
        let (start, end) = if (from.1, from.0) <= (to.1, to.0) {
            (from, to)
        } else {
            (to, from)
        };
        let mut out = String::new();
        for row in start.1..=end.1.min(self.height.saturating_sub(1)) {
            let first = if row == start.1 { start.0 } else { cols.0 };
            let last = if row == end.1 { end.0 } else { cols.1 };
            let mut line = String::new();
            for col in first..=last.min(self.width.saturating_sub(1)) {
                if let Some(cell) = self.cell(col, row) {
                    if !cell.skip {
                        line.push_str(if cell.symbol.is_empty() {
                            " "
                        } else {
                            &cell.symbol
                        });
                    }
                }
            }
            out.push_str(line.trim_end());
            if row != end.1 {
                out.push('\n');
            }
        }
        out
    }

    /// The run of non-blank cells around `at` on its row, kept inside the pane's
    /// columns `cols`: what a long press selects. A blank cell is its own word.
    pub fn word_at(&self, at: (u16, u16), cols: (u16, u16)) -> ((u16, u16), (u16, u16)) {
        let blank = |col: u16| {
            self.cell(col, at.1)
                .is_none_or(|cell| !cell.skip && cell.symbol.trim().is_empty())
        };
        if blank(at.0) {
            return (at, at);
        }
        let mut first = at.0;
        while first > cols.0 && !blank(first - 1) {
            first -= 1;
        }
        let mut last = at.0;
        while last < cols.1 && !blank(last + 1) {
            last += 1;
        }
        ((first, at.1), (last, at.1))
    }

    /// The link under a cell, for a tap to open: the cell's OSC 8 hyperlink, or a
    /// link written in the text across that cell, kept inside the pane's columns and
    /// followed across rows it wraps over. Local `file:` links are not returned: they
    /// name files on the pane's machine, not on the phone.
    pub fn link_at(&self, col: u16, row: u16) -> Option<String> {
        let phone_openable = |target: &str| {
            links::openable(target) && !target.to_ascii_lowercase().starts_with("file:")
        };
        let cell = self.cell(col, row)?;
        if let Some(index) = cell.hyperlink {
            if let Some(target) = self.hyperlinks.get(index as usize) {
                if phone_openable(target) {
                    return Some(target.clone());
                }
            }
        }
        let (left, right) = self.pane_columns(col, row);
        let right = right.min(self.width.saturating_sub(1));
        // The pane's text, each char with the column it starts at; a wide glyph's
        // trailing skip cell belongs to the char before it.
        let rows: Vec<links::Row> = (0..self.height)
            .map(|r| {
                let mut chars = Vec::new();
                for c in left..=right {
                    let Some(cell) = self.cell(c, r) else { break };
                    if cell.skip {
                        continue;
                    }
                    if cell.symbol.is_empty() {
                        chars.push((' ', c));
                    } else {
                        chars.extend(cell.symbol.chars().map(|ch| (ch, c)));
                    }
                }
                chars
            })
            .collect();
        links::link_in_text(&rows, right, row as usize, col).filter(|link| phone_openable(link))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(rows: &[&str]) -> Grid {
        let width = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0) as u16;
        let mut cells = Vec::new();
        for row in rows {
            let mut n = 0;
            for ch in row.chars() {
                cells.push(cell(&ch.to_string()));
                n += 1;
            }
            while n < width {
                cells.push(cell(" "));
                n += 1;
            }
        }
        Grid {
            width,
            height: rows.len() as u16,
            cells,
            ..Grid::default()
        }
    }

    fn cell(symbol: &str) -> CellData {
        CellData {
            symbol: symbol.into(),
            fg: 0,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    #[test]
    fn a_hyperlinked_cell_opens_its_target() {
        let mut g = grid(&["click here now"]);
        g.hyperlinks = vec!["https://example.com/docs".into()];
        for col in 6..10 {
            g.cells[col].hyperlink = Some(0);
        }
        assert_eq!(g.link_at(7, 0).as_deref(), Some("https://example.com/docs"));
        assert_eq!(g.link_at(2, 0), None);
    }

    #[test]
    fn a_hyperlink_with_another_scheme_is_not_opened() {
        let mut g = grid(&["file"]);
        g.hyperlinks = vec!["file:///etc/passwd".into()];
        g.cells[0].hyperlink = Some(0);
        assert_eq!(g.link_at(0, 0), None);
    }

    #[test]
    fn a_plain_url_is_found_from_any_of_its_chars() {
        let line = "see https://example.com/a?b=1 for more";
        let g = grid(&[line]);
        let start = line.find("https").unwrap();
        let end = start + "https://example.com/a?b=1".len();
        for col in start..end {
            assert_eq!(
                g.link_at(col as u16, 0).as_deref(),
                Some("https://example.com/a?b=1"),
                "col {col}"
            );
        }
        assert_eq!(g.link_at((start - 1) as u16, 0), None);
        assert_eq!(g.link_at(end as u16, 0), None);
    }

    #[test]
    fn trailing_punctuation_is_not_part_of_the_url() {
        let g = grid(&["go to https://example.com."]);
        assert_eq!(g.link_at(10, 0).as_deref(), Some("https://example.com"));
        assert_eq!(g.link_at(25, 0), None);
        let g = grid(&["(http://example.com/x), ok"]);
        assert_eq!(g.link_at(5, 0).as_deref(), Some("http://example.com/x"));
        assert_eq!(g.link_at(21, 0), None);
        let g = grid(&["https://en.wikipedia.org/wiki/Rust_(language)"]);
        assert_eq!(
            g.link_at(3, 0).as_deref(),
            Some("https://en.wikipedia.org/wiki/Rust_(language)")
        );
    }

    #[test]
    fn the_second_url_on_a_row_is_found() {
        let g = grid(&["http://a.example b https://b.example"]);
        assert_eq!(g.link_at(2, 0).as_deref(), Some("http://a.example"));
        assert_eq!(g.link_at(20, 0).as_deref(), Some("https://b.example"));
    }

    #[test]
    fn a_url_wrapped_onto_the_next_row_is_found_from_either_row() {
        let g = grid(&["see (https://exampl", "e.org/x) and more"]);
        assert_eq!(g.link_at(10, 0).as_deref(), Some("https://example.org/x"));
        assert_eq!(g.link_at(2, 1).as_deref(), Some("https://example.org/x"));
        assert_eq!(g.link_at(12, 1), None);
    }

    #[test]
    fn a_row_that_stops_short_of_the_edge_does_not_join_the_next() {
        let g = grid(&["https://a.example    ", "next"]);
        assert_eq!(g.link_at(3, 0).as_deref(), Some("https://a.example"));
        assert_eq!(g.link_at(1, 1), None);
    }

    #[test]
    fn a_browser_settings_link_is_found() {
        let g = grid(&["open chrome://settings/performance now"]);
        assert_eq!(g.link_at(9, 0).as_deref(), Some("chrome://settings/performance"));
    }

    #[test]
    fn a_url_an_app_wrapped_with_an_indent_is_found_whole() {
        let g = grid(&[
            "  https://example.org/a-long-path-th",
            "  at-wraps?q=1",
        ]);
        let whole = Some("https://example.org/a-long-path-that-wraps?q=1");
        assert_eq!(g.link_at(10, 0).as_deref(), whole);
        assert_eq!(g.link_at(5, 1).as_deref(), whole);
    }

    #[test]
    fn text_without_a_link_has_none() {
        let g = grid(&["just some text https:// here"]);
        for col in 0..g.width {
            assert_eq!(g.link_at(col, 0), None);
        }
        assert_eq!(g.link_at(99, 0), None);
    }
}
