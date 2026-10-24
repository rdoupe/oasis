//! The pane-area cell grid the server renders, kept in sync from full surfaces and patches.

use oasis_core::embed::wire::{
    CellData, CursorState, PaneSurfaceFrame, PaneSurfacePane, PaneSurfacePatch,
    PaneSurfacePatchRow, PaneSurfaceSplit,
};

pub struct Grid {
    pub width: u16,
    pub height: u16,
    pub cells: Vec<CellData>,
    pub cursor: Option<CursorState>,
    pub panes: Vec<PaneSurfacePane>,
    pub splits: Vec<PaneSurfaceSplit>,
    pub hyperlinks: Vec<String>,
    boot_id: String,
    projection_revision: u64,
    surface_revision: u64,
    /// Rows changed since the view last rebuilt them; empty means nothing to redraw.
    dirty: Vec<bool>,
    all_dirty: bool,
}

pub enum PatchOutcome {
    Applied,
    /// Out of order or against another surface; the caller asks for a fresh full surface.
    Rejected,
}

impl Grid {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            cells: Vec::new(),
            cursor: None,
            panes: Vec::new(),
            splits: Vec::new(),
            hyperlinks: Vec::new(),
            boot_id: String::new(),
            projection_revision: 0,
            surface_revision: 0,
            dirty: Vec::new(),
            all_dirty: true,
        }
    }

    pub fn set_full(&mut self, surface: PaneSurfaceFrame) {
        let frame = surface.frame;
        self.width = frame.width;
        self.height = frame.height;
        self.cells = frame.cells;
        self.cursor = frame.cursor;
        self.hyperlinks = frame.hyperlinks;
        self.panes = surface.panes;
        self.splits = surface.splits;
        self.boot_id = surface.boot_id;
        self.projection_revision = surface.projection_revision;
        self.surface_revision = surface.surface_revision;
        self.dirty = vec![false; usize::from(self.height)];
        self.all_dirty = true;
    }

    pub fn apply_patch(&mut self, patch: PaneSurfacePatch) -> PatchOutcome {
        if patch.boot_id != self.boot_id
            || patch.projection_revision != self.projection_revision
            || patch.base_surface_revision != self.surface_revision
        {
            return PatchOutcome::Rejected;
        }
        let width = usize::from(self.width);
        for row in &patch.rows {
            let start = usize::from(row.y) * width + usize::from(row.x);
            let end = start + row.cells.len();
            if row.y >= self.height || usize::from(row.x) + row.cells.len() > width {
                return PatchOutcome::Rejected;
            }
            self.cells[start..end].clone_from_slice(&row.cells);
            self.dirty[usize::from(row.y)] = true;
        }
        for updated in patch.panes {
            if let Some(existing) = self.panes.iter_mut().find(|p| p.pane_id == updated.pane_id) {
                *existing = updated;
            }
        }
        self.mark_cursor_rows(&patch.cursor);
        self.cursor = patch.cursor;
        self.surface_revision = patch.surface_revision;
        PatchOutcome::Applied
    }

    /// Applies the client core's coalesced patch: full-width rows already merged in order.
    pub fn apply_rows(
        &mut self,
        surface_revision: u64,
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
            self.dirty[usize::from(row.y)] = true;
        }
        for updated in panes {
            if let Some(existing) = self.panes.iter_mut().find(|p| p.pane_id == updated.pane_id) {
                *existing = updated;
            }
        }
        self.mark_cursor_rows(&cursor);
        self.cursor = cursor;
        self.surface_revision = surface_revision;
    }

    /// Forgets the surface, e.g. when switching machines, until the next full one arrives.
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    fn mark_cursor_rows(&mut self, next: &Option<CursorState>) {
        for cursor in [&self.cursor, next].into_iter().flatten() {
            if let Some(row) = self.dirty.get_mut(usize::from(cursor.y)) {
                *row = true;
            }
        }
    }

    /// Hands out the rows to rebuild and clears the damage.
    pub fn take_damage(&mut self) -> Damage {
        if self.all_dirty {
            self.all_dirty = false;
            self.dirty.iter_mut().for_each(|row| *row = false);
            return Damage::Full;
        }
        let rows: Vec<u16> = self
            .dirty
            .iter()
            .enumerate()
            .filter(|(_, dirty)| **dirty)
            .map(|(row, _)| row as u16)
            .collect();
        self.dirty.iter_mut().for_each(|row| *row = false);
        Damage::Rows(rows)
    }

    pub fn invalidate(&mut self) {
        self.all_dirty = true;
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
}

pub enum Damage {
    Full,
    Rows(Vec<u16>),
}

/// Decoded cell color; the packing is `color_to_u32` in oasis's wire module.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color {
    Default,
    Palette(u8),
    Rgb(u8, u8, u8),
}

pub fn decode_color(packed: u32) -> Color {
    match packed >> 24 {
        0x00 => match packed & 0xFF {
            0 => Color::Default,
            // ratatui's named colors 1..=16 map onto ANSI palette entries 0..=15.
            n @ 1..=16 => Color::Palette((n - 1) as u8),
            _ => Color::Default,
        },
        0x01 => Color::Palette((packed & 0xFF) as u8),
        0x02 => Color::Rgb(
            ((packed >> 16) & 0xFF) as u8,
            ((packed >> 8) & 0xFF) as u8,
            (packed & 0xFF) as u8,
        ),
        _ => Color::Default,
    }
}

/// ratatui `Modifier` bits as packed on the wire; the top nibble is the underline style.
pub mod style {
    pub const BOLD: u16 = 1 << 0;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINED: u16 = 1 << 3;
    pub const REVERSED: u16 = 1 << 6;
    pub const HIDDEN: u16 = 1 << 7;
    pub const CROSSED_OUT: u16 = 1 << 8;
    pub const UNDERLINE_STYLE_MASK: u16 = 0xF000;
    pub const UNDERLINE_STYLE_SHIFT: u16 = 12;
}

#[cfg(test)]
mod tests {
    use super::*;

    use oasis_core::embed::wire::{
        FrameData, PaneSurfaceFrame, PaneSurfacePatch, PaneSurfacePatchRow,
    };

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

    fn grid_with(rows: &[&str]) -> Grid {
        let width = rows[0].chars().count() as u16;
        let cells = rows
            .iter()
            .flat_map(|r| r.chars().map(|c| cell(&c.to_string())))
            .collect();
        let mut grid = Grid::new();
        grid.set_full(PaneSurfaceFrame {
            boot_id: "b".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: FrameData {
                cells,
                width,
                height: rows.len() as u16,
                cursor: None,
                hyperlinks: vec![],
                graphics: vec![],
            },
            panes: vec![],
            splits: vec![],
            popup: None,
            graphics: Default::default(),
        });
        grid
    }

    #[test]
    fn copy_text_stays_inside_the_pane_columns() {
        let grid = grid_with(&["│ab  ▕", "│cd  ▕"]);
        assert_eq!(grid.text((1, 0), (4, 1), (1, 4)), "ab\ncd");
    }

    #[test]
    fn patches_apply_in_order_and_reject_stale_bases() {
        let mut grid = grid_with(&["aa", "bb"]);
        grid.take_damage();
        let patch = |base, next| PaneSurfacePatch {
            boot_id: "b".into(),
            projection_revision: 1,
            base_surface_revision: base,
            surface_revision: next,
            rows: vec![PaneSurfacePatchRow {
                x: 1,
                y: 1,
                cells: vec![cell("Z")],
            }],
            panes: vec![],
            cursor: None,
        };
        assert!(matches!(
            grid.apply_patch(patch(1, 2)),
            PatchOutcome::Applied
        ));
        assert_eq!(grid.cell(1, 1).unwrap().symbol, "Z");
        assert!(matches!(grid.take_damage(), Damage::Rows(rows) if rows == vec![1]));
        assert!(matches!(
            grid.apply_patch(patch(1, 2)),
            PatchOutcome::Rejected
        ));
    }

    #[test]
    fn decodes_packed_colors() {
        assert_eq!(decode_color(0), Color::Default);
        assert_eq!(decode_color(0x01), Color::Palette(0));
        assert_eq!(decode_color(0x10), Color::Palette(15));
        assert_eq!(decode_color(0x01_00_00_d0), Color::Palette(0xd0));
        assert_eq!(decode_color(0x02_12_34_56), Color::Rgb(0x12, 0x34, 0x56));
    }
}
