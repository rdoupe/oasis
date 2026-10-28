//! The pane-area cell grid the server renders, kept in sync from the core's full
//! surfaces and coalesced patches.

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
}
