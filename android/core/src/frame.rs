//! The grid packed for the Kotlin view: per row, runs of cells that draw with one
//! `drawText` call, with colors already resolved to ARGB and styles to flags.
//!
//! Layout, big-endian: `u16 width, u16 height, i16 cursor col, i16 cursor row`
//! (-1 when hidden), `u8 cursor shape`, then per row `u16 runs` and per run
//! `u16 col, u16 cells, i32 fg, i32 bg, u16 flags, u16 len, len bytes of UTF-8`.

use oasis_core::embed::cell::{decode_color, decode_style, Color, Style};
use unicode_width::UnicodeWidthStr;

use crate::grid::Grid;

pub const BOLD: u16 = 1;
pub const ITALIC: u16 = 2;
pub const UNDERLINE: u16 = 4;
pub const STRIKE: u16 = 8;

pub struct Palette {
    pub foreground: u32,
    pub background: u32,
    pub colors: [u32; 256],
}

impl Default for Palette {
    /// Tokyo Night, Omarchy's default and clawmux's fallback.
    fn default() -> Self {
        let ansi = [
            0x1a1b26, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xad8ee6, 0x449dab, 0xa9b1d6,
            0x414868, 0xff7a93, 0xb9f27c, 0xff9e64, 0x7da6ff, 0xbb9af7, 0x0db9d7, 0xc0caf5,
        ];
        let mut colors = [0u32; 256];
        colors[..16].copy_from_slice(&ansi);
        let steps = [0u32, 95, 135, 175, 215, 255];
        for i in 0..216 {
            colors[16 + i] = steps[i / 36] << 16 | steps[(i / 6) % 6] << 8 | steps[i % 6];
        }
        for i in 0..24 {
            let level = 8 + 10 * i as u32;
            colors[232 + i] = level << 16 | level << 8 | level;
        }
        Self {
            foreground: 0xa9b1d6,
            background: 0x1a1b26,
            colors,
        }
    }
}

impl Palette {
    fn resolve(&self, packed: u32, default: u32) -> u32 {
        0xFF00_0000
            | match decode_color(packed) {
                Color::Reset => default,
                Color::Named(n) => self.colors[usize::from(n.saturating_sub(1))],
                Color::Indexed(n) => self.colors[usize::from(n)],
                Color::Rgb(r, g, b) => u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b),
            }
    }
}

struct Run {
    col: u16,
    cells: u16,
    fg: u32,
    bg: u32,
    flags: u16,
    text: String,
}

pub fn encode(grid: &Grid, palette: &Palette) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::from(grid.width) * usize::from(grid.height) + 64);
    out.extend_from_slice(&grid.width.to_be_bytes());
    out.extend_from_slice(&grid.height.to_be_bytes());
    let cursor = grid.cursor.as_ref().filter(|cursor| cursor.visible);
    let (col, row) = cursor.map_or((-1i16, -1i16), |c| (c.x as i16, c.y as i16));
    out.extend_from_slice(&col.to_be_bytes());
    out.extend_from_slice(&row.to_be_bytes());
    out.push(cursor.map_or(0, |c| c.shape));
    let mut runs: Vec<Run> = Vec::new();
    for y in 0..grid.height {
        runs.clear();
        let mut x = 0;
        while x < grid.width {
            let Some(cell) = grid.cell(x, y) else { break };
            x += 1;
            if cell.skip {
                continue;
            }
            let (style, underline) = decode_style(cell.modifier);
            let mut fg = palette.resolve(cell.fg, palette.foreground);
            let mut bg = palette.resolve(cell.bg, palette.background);
            if style.has(Style::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if style.has(Style::DIM) {
                fg = dim(fg, bg);
            }
            if style.has(Style::HIDDEN) {
                fg = bg;
            }
            let mut flags = 0;
            if style.has(Style::BOLD) {
                flags |= BOLD;
            }
            if style.has(Style::ITALIC) {
                flags |= ITALIC;
            }
            if underline.is_some() || cell.hyperlink.is_some() {
                flags |= UNDERLINE;
            }
            if style.has(Style::CROSSED_OUT) {
                flags |= STRIKE;
            }
            let symbol: &str = if cell.symbol.is_empty() {
                " "
            } else {
                &cell.symbol
            };
            let plain = symbol.len() == 1 && symbol.as_bytes()[0].is_ascii_graphic()
                || symbol == " ";
            let col = x - 1;
            if plain {
                if let Some(last) = runs.last_mut() {
                    if last.col + last.cells == col
                        && last.fg == fg
                        && last.bg == bg
                        && last.flags == flags
                        && last.text.len() == usize::from(last.cells)
                    {
                        last.cells += 1;
                        last.text.push_str(symbol);
                        continue;
                    }
                }
            }
            // A wide glyph covers the next cell too; it draws alone, so the
            // font's own advance never shifts the cells after it.
            let width = (symbol.width() as u16).clamp(1, 2);
            if width == 2 {
                x = (x + 1).min(grid.width);
            }
            runs.push(Run {
                col,
                cells: width.min(grid.width - col),
                fg,
                bg,
                flags,
                text: symbol.to_owned(),
            });
        }
        out.extend_from_slice(&(runs.len() as u16).to_be_bytes());
        for run in &runs {
            out.extend_from_slice(&run.col.to_be_bytes());
            out.extend_from_slice(&run.cells.to_be_bytes());
            out.extend_from_slice(&run.fg.to_be_bytes());
            out.extend_from_slice(&run.bg.to_be_bytes());
            out.extend_from_slice(&run.flags.to_be_bytes());
            out.extend_from_slice(&(run.text.len() as u16).to_be_bytes());
            out.extend_from_slice(run.text.as_bytes());
        }
    }
    out
}

/// Halfway from `fg` toward `bg`, as terminals draw faint text.
fn dim(fg: u32, bg: u32) -> u32 {
    let mix = |shift: u32| ((((fg >> shift) & 0xFF) + ((bg >> shift) & 0xFF)) / 2) << shift;
    0xFF00_0000 | mix(16) | mix(8) | mix(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oasis_core::embed::wire::CellData;

    fn cell(symbol: &str, fg: u32) -> CellData {
        CellData {
            symbol: symbol.into(),
            fg,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    fn row_runs(bytes: &[u8]) -> Vec<(u16, u16, String)> {
        let mut at = 9;
        let read16 = |at: &mut usize| {
            let v = u16::from_be_bytes([bytes[*at], bytes[*at + 1]]);
            *at += 2;
            v
        };
        let count = read16(&mut at);
        (0..count)
            .map(|_| {
                let col = read16(&mut at);
                let cells = read16(&mut at);
                at += 10;
                let len = usize::from(read16(&mut at));
                let text = String::from_utf8(bytes[at..at + len].to_vec()).unwrap();
                at += len;
                (col, cells, text)
            })
            .collect()
    }

    #[test]
    fn plain_cells_of_one_style_share_a_run_and_wide_glyphs_stand_alone() {
        let red = 0x0000_0002;
        let mut cells = vec![cell("a", 0), cell("b", 0), cell("c", red), cell("界", 0)];
        cells.push(cell("", 0));
        cells.push(cell("d", 0));
        let grid = Grid {
            width: 6,
            height: 1,
            cells,
            ..Grid::default()
        };
        assert_eq!(
            row_runs(&encode(&grid, &Palette::default())),
            vec![
                (0, 2, "ab".to_owned()),
                (2, 1, "c".to_owned()),
                (3, 2, "界".to_owned()),
                (5, 1, "d".to_owned()),
            ]
        );
    }

    #[test]
    fn colors_resolve_through_the_palette() {
        let palette = Palette::default();
        assert_eq!(palette.resolve(0, palette.foreground), 0xFFA9_B1D6);
        assert_eq!(palette.resolve(0x0000_0002, 0), 0xFFF7_768E, "named red");
        assert_eq!(palette.resolve(0x0100_00E8, 0), 0xFF080808, "grey ramp");
        assert_eq!(palette.resolve(0x0212_3456, 0), 0xFF123456);
    }
}
