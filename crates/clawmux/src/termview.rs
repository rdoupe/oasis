//! The terminal surface widget: draws the server-rendered pane grid with GTK's GPU renderer.
//!
//! Each grid row is cached as a render node and rebuilt only when the server patches it,
//! so steady output costs one row rebuild per changed row and an idle view costs nothing.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk, pango};

use crate::grid::{decode_color, style, Color, Damage, Grid};
use crate::theme::{Rgb, Theme};

pub const PAD: f64 = 6.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Metrics {
    pub cell_w: f64,
    pub cell_h: f64,
    pub baseline: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TermView {
        pub grid: RefCell<Option<Rc<RefCell<Grid>>>>,
        pub theme: RefCell<Option<Theme>>,
        pub font: RefCell<Option<pango::FontDescription>>,
        pub metrics: Cell<Metrics>,
        pub rows: RefCell<Vec<Option<gsk::RenderNode>>>,
        pub size_cells: Cell<(u16, u16)>,
        pub on_resize: RefCell<Option<Box<dyn Fn(u16, u16)>>>,
        pub selection: Cell<Option<Selection>>,
        pub window_focused: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TermView {
        const NAME: &'static str = "ClawmuxTermView";
        type Type = super::TermView;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for TermView {}

    impl WidgetImpl for TermView {
        fn measure(&self, _orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            (80, 400, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.obj().update_size(width, height);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct TermView(ObjectSubclass<imp::TermView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TermView {
    pub fn new(grid: Rc<RefCell<Grid>>, theme: Theme) -> Self {
        let view: Self = glib::Object::new();
        view.set_hexpand(true);
        view.set_vexpand(true);
        view.set_focusable(true);
        view.set_can_focus(true);
        view.set_overflow(gtk::Overflow::Hidden);
        view.imp().grid.replace(Some(grid));
        view.imp().window_focused.set(true);
        view.set_theme(theme);
        view
    }

    pub fn set_theme(&self, theme: Theme) {
        let font =
            pango::FontDescription::from_string(&format!("{} {}", theme.font_family, theme.font_size));
        self.imp().font.replace(Some(font));
        self.imp().theme.replace(Some(theme));
        self.measure_font();
        self.invalidate();
    }

    pub fn connect_resize(&self, callback: impl Fn(u16, u16) + 'static) {
        self.imp().on_resize.replace(Some(Box::new(callback)));
    }

    pub fn metrics(&self) -> Metrics {
        self.imp().metrics.get()
    }

    pub fn size_cells(&self) -> (u16, u16) {
        self.imp().size_cells.get()
    }

    pub fn set_window_focused(&self, focused: bool) {
        self.imp().window_focused.set(focused);
        self.queue_draw();
    }

    pub fn selection(&self) -> Option<Selection> {
        self.imp().selection.get()
    }

    pub fn set_selection(&self, selection: Option<Selection>) {
        if self.imp().selection.get() != selection {
            self.imp().selection.set(selection);
            self.queue_draw();
        }
    }

    /// Drops every cached row; the next frame rebuilds them all.
    pub fn invalidate(&self) {
        self.imp().rows.borrow_mut().clear();
        self.queue_draw();
    }

    /// Converts a widget-relative point to a grid cell.
    pub fn cell_at(&self, x: f64, y: f64) -> (u16, u16) {
        let m = self.metrics();
        let (cols, rows) = self.size_cells();
        let col = ((x - PAD) / m.cell_w).floor().clamp(0.0, f64::from(cols.saturating_sub(1)));
        let row = ((y - PAD) / m.cell_h).floor().clamp(0.0, f64::from(rows.saturating_sub(1)));
        (col as u16, row as u16)
    }

    fn measure_font(&self) {
        let imp = self.imp();
        let font = imp.font.borrow();
        let Some(font) = font.as_ref() else { return };
        let layout = pango::Layout::new(&self.pango_context());
        layout.set_font_description(Some(font));
        layout.set_text("MMMMMMMMMM");
        let (_, logical) = layout.extents();
        let scale = f64::from(pango::SCALE);
        let metrics = Metrics {
            cell_w: f64::from(logical.width()) / scale / 10.0,
            cell_h: (f64::from(logical.height()) / scale).ceil(),
            baseline: f64::from(layout.baseline()) / scale,
        };
        imp.metrics.set(metrics);
        let (w, h) = (self.width(), self.height());
        if w > 0 && h > 0 {
            imp.size_cells.set((0, 0));
            self.update_size(w, h);
        }
    }

    fn update_size(&self, width: i32, height: i32) {
        let m = self.metrics();
        if m.cell_w <= 0.0 || m.cell_h <= 0.0 {
            return;
        }
        let cols = ((f64::from(width) - 2.0 * PAD) / m.cell_w).floor().max(1.0) as u16;
        let rows = ((f64::from(height) - 2.0 * PAD) / m.cell_h).floor().max(1.0) as u16;
        if self.imp().size_cells.replace((cols, rows)) != (cols, rows) {
            if let Some(callback) = self.imp().on_resize.borrow().as_ref() {
                callback(cols, rows);
            }
        }
    }

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let imp = self.imp();
        let grid_rc = imp.grid.borrow().clone();
        let theme_ref = imp.theme.borrow();
        let (Some(grid_rc), Some(theme)) = (grid_rc, theme_ref.as_ref()) else {
            return;
        };
        let m = self.metrics();
        let bounds = graphene::Rect::new(0.0, 0.0, self.width() as f32, self.height() as f32);
        snapshot.append_color(&theme.background.rgba(1.0), &bounds);

        let mut grid = grid_rc.borrow_mut();
        let mut rows = imp.rows.borrow_mut();
        if rows.len() != usize::from(grid.height) {
            rows.clear();
            rows.resize(usize::from(grid.height), None);
        }
        match grid.take_damage() {
            Damage::Full => rows.iter_mut().for_each(|row| *row = None),
            Damage::Rows(damaged) => {
                for row in damaged {
                    if let Some(slot) = rows.get_mut(usize::from(row)) {
                        *slot = None;
                    }
                }
            }
        }
        let font = imp.font.borrow();
        let Some(font) = font.as_ref() else { return };
        let ctx = self.pango_context();
        for (y, slot) in rows.iter_mut().enumerate() {
            if slot.is_none() {
                *slot = build_row(&grid, y as u16, theme, font, &ctx, m);
            }
            if let Some(node) = slot {
                snapshot.save();
                snapshot.translate(&graphene::Point::new(
                    PAD as f32,
                    (PAD + y as f64 * m.cell_h) as f32,
                ));
                snapshot.append_node(node);
                snapshot.restore();
            }
        }
        if let Some(selection) = imp.selection.get() {
            draw_selection(snapshot, &grid, selection, theme, m);
        }
        draw_cursor(snapshot, &grid, theme, font, &ctx, m, imp.window_focused.get());
    }
}

fn cell_colors(cell: &oasis_core::embed::wire::CellData, theme: &Theme) -> (Rgb, Rgb) {
    let resolve = |color: Color, default: Rgb| match color {
        Color::Default => default,
        Color::Palette(index) => theme.palette[usize::from(index)],
        Color::Rgb(r, g, b) => Rgb(r, g, b),
    };
    let mut fg = resolve(decode_color(cell.fg), theme.foreground);
    let mut bg = resolve(decode_color(cell.bg), theme.background);
    if cell.modifier & style::REVERSED != 0 {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.modifier & style::DIM != 0 {
        fg = blend(fg, bg, 0.5);
    }
    (fg, bg)
}

fn blend(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let mix = |x: u8, y: u8| (f32::from(x) * (1.0 - t) + f32::from(y) * t).round() as u8;
    Rgb(mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> graphene::Rect {
    graphene::Rect::new(x as f32, y as f32, w as f32, h as f32)
}

/// Whether a glyph can share a layout run with its neighbours and still land on its cell.
fn runs_inline(symbol: &str) -> bool {
    let mut chars = symbol.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => (c as u32) < 0x2500 && unicode_width::UnicodeWidthChar::width(c) == Some(1),
        _ => false,
    }
}

struct TextRun {
    col: u16,
    text: String,
    attrs: pango::AttrList,
}

fn build_row(
    grid: &Grid,
    row: u16,
    theme: &Theme,
    font: &pango::FontDescription,
    ctx: &pango::Context,
    m: Metrics,
) -> Option<gsk::RenderNode> {
    let snapshot = gtk::Snapshot::new();
    let width = grid.width;

    // Backgrounds, merged into spans so adjacent cells leave no seams.
    let mut span: Option<(u16, Rgb)> = None;
    for col in 0..=width {
        let bg = grid.cell(col, row).map(|cell| cell_colors(cell, theme).1);
        let same = matches!((span, bg), (Some((_, a)), Some(b)) if a == b);
        if !same {
            if let Some((start, color)) = span.take() {
                if color != theme.background {
                    snapshot.append_color(
                        &color.rgba(1.0),
                        &rect(f64::from(start) * m.cell_w, 0.0, f64::from(col - start) * m.cell_w, m.cell_h),
                    );
                }
            }
            span = bg.map(|bg| (col, bg));
        }
    }

    // Text: inline runs for ordinary glyphs, single placed glyphs for wide or symbol cells.
    let mut runs: Vec<TextRun> = Vec::new();
    let mut current: Option<TextRun> = None;
    for col in 0..width {
        let Some(cell) = grid.cell(col, row) else { break };
        if cell.skip {
            continue;
        }
        let symbol = if cell.symbol.is_empty() { " " } else { cell.symbol.as_str() };
        let (fg, _) = cell_colors(cell, theme);
        if cell.modifier & style::HIDDEN != 0 || symbol == " " {
            if let Some(run) = current.as_mut() {
                run.text.push(' ');
            }
            continue;
        }
        if let Some(c) = symbol.chars().next().filter(|_| symbol.chars().count() == 1) {
            if draw_box(&snapshot, c, f64::from(col) * m.cell_w, m, fg) {
                if let Some(run) = current.as_mut() {
                    run.text.push(' ');
                }
                continue;
            }
        }
        if !runs_inline(symbol) {
            if let Some(run) = current.take() {
                runs.push(run);
            }
            let mut single = TextRun {
                col,
                text: String::new(),
                attrs: pango::AttrList::new(),
            };
            push_styled(&mut single, symbol, cell.modifier, fg);
            runs.push(single);
            continue;
        }
        let run = current.get_or_insert_with(|| TextRun {
            col,
            text: String::new(),
            attrs: pango::AttrList::new(),
        });
        push_styled(run, symbol, cell.modifier, fg);
    }
    if let Some(run) = current.take() {
        runs.push(run);
    }
    for run in runs {
        if run.text.trim().is_empty() {
            continue;
        }
        let layout = pango::Layout::new(ctx);
        layout.set_font_description(Some(font));
        layout.set_text(&run.text);
        layout.set_attributes(Some(&run.attrs));
        snapshot.save();
        snapshot.translate(&graphene::Point::new((f64::from(run.col) * m.cell_w) as f32, 0.0));
        snapshot.append_layout(&layout, &theme.foreground.rgba(1.0));
        snapshot.restore();
    }
    snapshot.to_node()
}

fn push_styled(run: &mut TextRun, symbol: &str, modifier: u16, fg: Rgb) {
    let start = run.text.len() as u32;
    run.text.push_str(symbol);
    let end = run.text.len() as u32;
    let scale = |v: u8| u16::from(v) * 257;
    let add = |mut attr: pango::Attribute| {
        attr.set_start_index(start);
        attr.set_end_index(end);
        run.attrs.insert(attr);
    };
    add(pango::AttrColor::new_foreground(scale(fg.0), scale(fg.1), scale(fg.2)).into());
    if modifier & style::BOLD != 0 {
        add(pango::AttrInt::new_weight(pango::Weight::Bold).into());
    }
    if modifier & style::ITALIC != 0 {
        add(pango::AttrInt::new_style(pango::Style::Italic).into());
    }
    let underline = (modifier & style::UNDERLINE_STYLE_MASK) >> style::UNDERLINE_STYLE_SHIFT;
    if modifier & style::UNDERLINED != 0 || underline != 0 {
        let kind = match underline {
            2 => pango::Underline::Double,
            3 => pango::Underline::Error,
            _ => pango::Underline::Single,
        };
        add(pango::AttrInt::new_underline(kind).into());
    }
    if modifier & style::CROSSED_OUT != 0 {
        add(pango::AttrInt::new_strikethrough(true).into());
    }
}

/// Draws light box-drawing glyphs as exact lines so borders join across cells and rows.
fn draw_box(snapshot: &gtk::Snapshot, c: char, x: f64, m: Metrics, fg: Rgb) -> bool {
    let thick = (m.cell_w / 8.0).round().max(1.0);
    let color = fg.rgba(1.0);
    // Dashed lines: segments sized to the cell so they line up across cells.
    let dashes = match c {
        '┄' | '┅' | '┈' | '┉' | '╌' | '╍' => Some((true, if matches!(c, '╌' | '╍') { 2 } else if matches!(c, '┄' | '┅') { 3 } else { 4 })),
        '┆' | '┇' | '┊' | '┋' | '╎' | '╏' => Some((false, if matches!(c, '╎' | '╏') { 2 } else if matches!(c, '┆' | '┇') { 3 } else { 4 })),
        _ => None,
    };
    if let Some((horizontal, count)) = dashes {
        let cx = x + (m.cell_w / 2.0).floor();
        let cy = (m.cell_h / 2.0).floor();
        let span = if horizontal { m.cell_w } else { m.cell_h };
        let step = span / f64::from(count);
        for i in 0..count {
            let start = f64::from(i) * step;
            let len = (step * 0.6).max(1.0);
            let r = if horizontal {
                rect(x + start, cy, len, thick)
            } else {
                rect(cx, start, thick, len)
            };
            snapshot.append_color(&color, &r);
        }
        return true;
    }
    // (up, down, left, right)
    let arms = match c {
        '╴' => (false, false, true, false),
        '╵' => (true, false, false, false),
        '╶' => (false, false, false, true),
        '╷' => (false, true, false, false),
        '─' | '━' | '═' => (false, false, true, true),
        '│' | '┃' | '║' => (true, true, false, false),
        '┌' | '╭' | '┏' | '╔' => (false, true, false, true),
        '┐' | '╮' | '┓' | '╗' => (false, true, true, false),
        '└' | '╰' | '┗' | '╚' => (true, false, false, true),
        '┘' | '╯' | '┛' | '╝' => (true, false, true, false),
        '├' | '┣' | '╠' => (true, true, false, true),
        '┤' | '┫' | '╣' => (true, true, true, false),
        '┬' | '┳' | '╦' => (false, true, true, true),
        '┴' | '┻' | '╩' => (true, false, true, true),
        '┼' | '╋' | '╬' => (true, true, true, true),
        _ => return false,
    };
    let cx = x + (m.cell_w / 2.0).floor();
    let cy = (m.cell_h / 2.0).floor();
    if arms.0 {
        snapshot.append_color(&color, &rect(cx, 0.0, thick, cy + thick));
    }
    if arms.1 {
        snapshot.append_color(&color, &rect(cx, cy, thick, m.cell_h - cy));
    }
    if arms.2 {
        snapshot.append_color(&color, &rect(x, cy, cx - x + thick, thick));
    }
    if arms.3 {
        snapshot.append_color(&color, &rect(cx, cy, x + m.cell_w - cx, thick));
    }
    true
}

fn draw_selection(snapshot: &gtk::Snapshot, grid: &Grid, selection: Selection, theme: &Theme, m: Metrics) {
    let (start, end) = if (selection.anchor.1, selection.anchor.0) <= (selection.head.1, selection.head.0) {
        (selection.anchor, selection.head)
    } else {
        (selection.head, selection.anchor)
    };
    let color = theme.selection.rgba(0.7);
    for row in start.1..=end.1.min(grid.height.saturating_sub(1)) {
        let first = if row == start.1 { start.0 } else { 0 };
        let last = if row == end.1 { end.0 } else { grid.width.saturating_sub(1) };
        snapshot.append_color(
            &color,
            &rect(
                PAD + f64::from(first) * m.cell_w,
                PAD + f64::from(row) * m.cell_h,
                f64::from(last - first + 1) * m.cell_w,
                m.cell_h,
            ),
        );
    }
}

fn draw_cursor(
    snapshot: &gtk::Snapshot,
    grid: &Grid,
    theme: &Theme,
    font: &pango::FontDescription,
    ctx: &pango::Context,
    m: Metrics,
    focused: bool,
) {
    let Some(cursor) = grid.cursor.as_ref().filter(|c| c.visible) else { return };
    let x = PAD + f64::from(cursor.x) * m.cell_w;
    let y = PAD + f64::from(cursor.y) * m.cell_h;
    let color = theme.cursor.rgba(1.0);
    if !focused {
        let t = 1.0;
        for r in [
            rect(x, y, m.cell_w, t),
            rect(x, y + m.cell_h - t, m.cell_w, t),
            rect(x, y, t, m.cell_h),
            rect(x + m.cell_w - t, y, t, m.cell_h),
        ] {
            snapshot.append_color(&color, &r);
        }
        return;
    }
    match cursor.shape {
        3 | 4 => snapshot.append_color(&color, &rect(x, y + m.cell_h - 2.0, m.cell_w, 2.0)),
        5 | 6 => snapshot.append_color(&color, &rect(x, y, 2.0, m.cell_h)),
        _ => {
            snapshot.append_color(&color, &rect(x, y, m.cell_w, m.cell_h));
            if let Some(cell) = grid.cell(cursor.x, cursor.y) {
                if !cell.symbol.trim().is_empty() {
                    let layout = pango::Layout::new(ctx);
                    layout.set_font_description(Some(font));
                    layout.set_text(&cell.symbol);
                    snapshot.save();
                    snapshot.translate(&graphene::Point::new(x as f32, y as f32));
                    snapshot.append_layout(&layout, &theme.background.rgba(1.0));
                    snapshot.restore();
                }
            }
        }
    }
}
