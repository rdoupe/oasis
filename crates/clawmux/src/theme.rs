//! Colors and fonts from the active Omarchy theme and the user's Ghostty font, so the
//! window matches the rest of the desktop.

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn parse(text: &str) -> Option<Self> {
        let hex = text.trim().trim_matches('"').trim_start_matches('#');
        if hex.len() != 6 {
            return None;
        }
        let value = u32::from_str_radix(hex, 16).ok()?;
        Some(Self((value >> 16) as u8, (value >> 8) as u8, value as u8))
    }

    pub fn css(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    pub fn rgba(self, alpha: f32) -> gtk::gdk::RGBA {
        gtk::gdk::RGBA::new(
            f32::from(self.0) / 255.0,
            f32::from(self.1) / 255.0,
            f32::from(self.2) / 255.0,
            alpha,
        )
    }
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub palette: [Rgb; 256],
    pub accent: Rgb,
    pub muted: Rgb,
    pub sidebar: Rgb,
    pub raised: Rgb,
    pub dim_foreground: Rgb,
    pub red: Rgb,
    pub yellow: Rgb,
    pub green: Rgb,
    pub font_family: String,
    pub font_size: f64,
}

pub fn theme_dir() -> PathBuf {
    home().join(".local/state/omarchy/current/theme")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

impl Theme {
    /// Tokyo Night, Omarchy's default, for machines without a current theme.
    fn fallback() -> Self {
        let ansi = [
            "#1a1b26", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#ad8ee6", "#449dab", "#a9b1d6",
            "#414868", "#ff7a93", "#b9f27c", "#ff9e64", "#7da6ff", "#bb9af7", "#0db9d7", "#c0caf5",
        ];
        let mut palette = xterm_palette();
        for (slot, hex) in palette.iter_mut().zip(ansi) {
            *slot = Rgb::parse(hex).unwrap();
        }
        let c = |hex| Rgb::parse(hex).unwrap();
        Self {
            background: c("#1a1b26"),
            foreground: c("#a9b1d6"),
            cursor: c("#c0caf5"),
            selection: c("#292e42"),
            palette,
            accent: c("#7aa2f7"),
            muted: c("#414868"),
            sidebar: c("#13141c"),
            raised: c("#24283b"),
            dim_foreground: c("#565f89"),
            red: c("#f7768e"),
            yellow: c("#e0af68"),
            green: c("#9ece6a"),
            font_family: "JetBrainsMono Nerd Font".into(),
            font_size: 11.0,
        }
    }

    pub fn load() -> Self {
        let mut theme = Self::fallback();
        let dir = theme_dir();
        if let Ok(text) = std::fs::read_to_string(dir.join("ghostty.conf")) {
            theme.apply_ghostty(&text);
        }
        if let Ok(text) = std::fs::read_to_string(dir.join("colors.toml")) {
            theme.apply_colors_toml(&text);
        }
        if let Ok(text) = std::fs::read_to_string(home().join(".config/ghostty/config")) {
            theme.apply_ghostty_font(&text);
        }
        if let Ok(family) = std::env::var("CLAWMUX_FONT") {
            theme.font_family = family;
        }
        if let Some(size) = std::env::var("CLAWMUX_FONT_SIZE")
            .ok()
            .and_then(|size| size.parse().ok())
        {
            theme.font_size = size;
        }
        if let Some(size) = load_font_size() {
            theme.font_size = size;
        }
        theme
    }

    fn apply_ghostty(&mut self, text: &str) {
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            match key {
                "background" => set(&mut self.background, value),
                "foreground" => set(&mut self.foreground, value),
                "cursor-color" => set(&mut self.cursor, value),
                "selection-background" => set(&mut self.selection, value),
                "palette" => {
                    if let Some((index, color)) = value.split_once('=') {
                        if let (Ok(index), Some(color)) =
                            (index.trim().parse::<u8>(), Rgb::parse(color))
                        {
                            self.palette[usize::from(index)] = color;
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn apply_colors_toml(&mut self, text: &str) {
        let Ok(table) = text.parse::<toml::Table>() else {
            return;
        };
        let get = |key: &str| table.get(key).and_then(|v| v.as_str()).and_then(Rgb::parse);
        let slots: [(&str, &mut Rgb); 8] = [
            ("accent", &mut self.accent),
            ("muted", &mut self.muted),
            ("dark_background", &mut self.sidebar),
            ("lighter_background", &mut self.raised),
            ("dark_foreground", &mut self.dim_foreground),
            ("red", &mut self.red),
            ("yellow", &mut self.yellow),
            ("green", &mut self.green),
        ];
        for (key, slot) in slots {
            if let Some(color) = get(key) {
                *slot = color;
            }
        }
    }

    fn apply_ghostty_font(&mut self, text: &str) {
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"');
            match key.trim() {
                "font-family" if !value.is_empty() => self.font_family = value.to_owned(),
                // Ghostty and Pango both size fonts in points.
                "font-size" => {
                    if let Ok(size) = value.parse::<f64>() {
                        self.font_size = size.max(6.0);
                    }
                }
                _ => {}
            }
        }
    }

    pub fn css(&self) -> String {
        format!(
            r#"
window.clawmux {{ background: {bg}; color: {fg}; }}
.clawmux-sidebar {{ background: {sidebar}; border-right: 1px solid {raised}; }}
.clawmux-sidebar .heading {{ color: {accent}; font-size: 0.74em; font-weight: 800; letter-spacing: 0.12em; margin: 12px 14px 4px; }}
.clawmux-sidebar list {{ background: transparent; }}
.clawmux-sidebar row {{ border-radius: 8px; margin: 1px 8px; padding: 5px 8px; color: {fg}; background: transparent; }}
.clawmux-sidebar row:hover {{ background: alpha({raised}, 0.7); }}
.clawmux-sidebar row.focused {{ background: {raised}; box-shadow: inset 3px 0 0 {accent}; }}
.clawmux-sidebar .meta {{ color: {dim}; font-size: 0.86em; }}
.clawmux-sidebar .number {{ color: {dim}; font-feature-settings: "tnum"; min-width: 1.2em; }}
.clawmux-status {{ color: {dim}; font-size: 0.82em; padding: 8px 14px; border-top: 1px solid {raised}; }}
.clawmux-tabs {{ background: {sidebar}; padding: 3px 6px; border-bottom: 1px solid {raised}; font-size: 0.85em; }}
.clawmux-tabs .tab-pill {{ border-radius: 6px; color: alpha({fg}, 0.6); }}
.clawmux-tabs .tab-pill:hover {{ background: alpha({fg}, 0.07); color: {fg}; }}
.clawmux-tabs .tab-pill.active {{ background: alpha({fg}, 0.14); color: {fg}; }}
.clawmux-tabs .tab-label {{ padding: 2px 1px 2px 9px; font-weight: 700; }}
.clawmux-tabs .tab-pill > stack {{ min-width: 18px; margin-right: 3px; }}
.clawmux-tabs .tab-pill .dot {{ min-width: 6px; min-height: 6px; border-radius: 3px; }}
.clawmux-tabs button.tab-action {{ background: none; border: none; box-shadow: none; outline: none; min-height: 0; min-width: 16px; padding: 0; color: alpha({fg}, 0.55); }}
.clawmux-tabs button.tab-action:hover {{ color: {fg}; }}
.clawmux-tabs button.tab-add {{ background: none; border: none; box-shadow: none; min-height: 0; padding: 1px 8px; border-radius: 6px; color: alpha({fg}, 0.55); }}
.clawmux-tabs button.tab-add:hover {{ background: alpha({fg}, 0.07); color: {fg}; }}
.clawmux-banner {{ background: {raised}; color: {fg}; padding: 6px 12px; }}
.clawmux-stall {{ background: alpha({yellow}, 0.16); color: {yellow}; padding: 6px 12px; font-weight: 600; border-bottom: 1px solid alpha({yellow}, 0.4); }}
.clawmux-sidebar .not-responding {{ color: {yellow}; font-size: 0.86em; font-weight: 600; margin-right: 10px; }}
.clawmux-tabs button.sidebar-toggle {{ background: none; border: none; box-shadow: none; min-height: 0; padding: 1px 9px; border-radius: 6px; color: alpha({fg}, 0.6); }}
.clawmux-tabs button.sidebar-toggle:hover {{ background: alpha({fg}, 0.07); color: {fg}; }}
.clawmux-scrim {{ background: alpha(black, 0.45); }}
.clawmux-drawer .clawmux-sidebar {{ min-width: 300px; box-shadow: 4px 0 18px alpha(black, 0.5); }}
.touch .clawmux-sidebar row {{ padding: 11px 10px; }}
.touch .clawmux-tabs {{ padding: 4px 6px; }}
.touch .clawmux-tabs button.sidebar-toggle {{ min-width: 44px; min-height: 38px; font-size: 1.35em; }}
.touch .clawmux-tabs .tab-pill {{ min-height: 38px; }}
.touch .clawmux-tabs button.tab-add {{ min-width: 44px; min-height: 38px; }}
popover.touch-menu button {{ min-width: 72px; min-height: 44px; font-size: 1.15em; }}
.clawmux-sidebar row.heading-row {{ padding: 0; margin: 0; }}
.clawmux-sidebar separator.section-divider {{ background: {raised}; min-height: 1px; margin: 10px 12px 2px; }}
.clawmux-sidebar row.heading-row:hover {{ background: transparent; }}
.clawmux-sidebar row.stale {{ opacity: 0.55; }}
.clawmux-sidebar row.active-machine .machine {{ color: {accent}; font-weight: 700; }}
.clawmux-sidebar .machine {{ color: {dim}; font-weight: 600; }}
.clawmux-sidebar button.group-toggle {{ min-height: 0; min-width: 0; padding: 0 4px; color: {dim}; background: transparent; box-shadow: none; }}
.dot {{ min-width: 8px; min-height: 8px; border-radius: 4px; background: {muted}; }}
.dot.working {{ background: {yellow}; }}
.dot.blocked {{ background: {red}; }}
.dot.done {{ background: {green}; }}
.dot.idle {{ background: {muted}; }}
"#,
            bg = self.background.css(),
            fg = self.foreground.css(),
            sidebar = self.sidebar.css(),
            raised = self.raised.css(),
            dim = self.dim_foreground.css(),
            accent = self.accent.css(),
            muted = self.muted.css(),
            yellow = self.yellow.css(),
            red = self.red.css(),
            green = self.green.css(),
        )
    }
}

/// The size picked with ctrl+-/ctrl+=, kept across restarts; absent means the theme's.
fn font_size_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("clawmux/font-size")
}

fn load_font_size() -> Option<f64> {
    let size: f64 = std::fs::read_to_string(font_size_path())
        .ok()?
        .trim()
        .parse()
        .ok()?;
    (MIN_FONT_SIZE..=MAX_FONT_SIZE)
        .contains(&size)
        .then_some(size)
}

pub const MIN_FONT_SIZE: f64 = 5.0;
pub const MAX_FONT_SIZE: f64 = 40.0;

/// Saves the zoomed font size, or forgets it with `None` to go back to the theme's.
pub fn save_font_size(size: Option<f64>) {
    let path = font_size_path();
    let result = match size {
        Some(size) => path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|_| std::fs::write(&path, format!("{size}\n"))),
        None => match std::fs::remove_file(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    };
    if let Err(error) = result {
        eprintln!("clawmux: could not save font size: {error}");
    }
}

fn set(slot: &mut Rgb, value: &str) {
    if let Some(color) = Rgb::parse(value) {
        *slot = color;
    }
}

/// The xterm 256-color palette; entries 0..16 are replaced by the theme.
fn xterm_palette() -> [Rgb; 256] {
    let mut palette = [Rgb(0, 0, 0); 256];
    let steps = [0u8, 95, 135, 175, 215, 255];
    for i in 0..216 {
        palette[16 + i] = Rgb(steps[i / 36], steps[(i / 6) % 6], steps[i % 6]);
    }
    for i in 0..24 {
        let level = 8 + 10 * i as u8;
        palette[232 + i] = Rgb(level, level, level);
    }
    palette
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_omarchy_ghostty_theme() {
        let mut theme = Theme::fallback();
        theme.apply_ghostty("background = #000001\npalette = 3=#112233\nfont-size = 9\n");
        assert_eq!(theme.background, Rgb(0, 0, 1));
        assert_eq!(theme.palette[3], Rgb(0x11, 0x22, 0x33));
        assert_eq!(theme.palette[196], Rgb(255, 0, 0));
    }

    #[test]
    fn reads_colors_toml_accents() {
        let mut theme = Theme::fallback();
        theme.apply_colors_toml("accent = \"#010203\"\ndark_background = \"#0a0b0c\"\n");
        assert_eq!(theme.accent, Rgb(1, 2, 3));
        assert_eq!(theme.sidebar, Rgb(10, 11, 12));
    }
}
