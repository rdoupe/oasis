//! Colors and fonts from the active Omarchy theme and the user's Ghostty font, so the
//! window matches the rest of the desktop.

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
.clawmux-sidebar .heading {{ color: {dim}; font-size: 0.78em; font-weight: 700; letter-spacing: 0.08em; margin: 12px 14px 4px; }}
.clawmux-sidebar list {{ background: transparent; }}
.clawmux-sidebar row {{ border-radius: 8px; margin: 1px 8px; padding: 5px 8px; color: {fg}; background: transparent; }}
.clawmux-sidebar row:hover {{ background: alpha({raised}, 0.7); }}
.clawmux-sidebar row.focused {{ background: {raised}; box-shadow: inset 3px 0 0 {accent}; }}
.clawmux-sidebar .meta {{ color: {dim}; font-size: 0.86em; }}
.clawmux-sidebar .number {{ color: {dim}; font-feature-settings: "tnum"; min-width: 1.2em; }}
.clawmux-status {{ color: {dim}; font-size: 0.82em; padding: 8px 14px; border-top: 1px solid {raised}; }}
.clawmux-tabs {{ background: {sidebar}; padding: 4px 6px; border-bottom: 1px solid {raised}; }}
.clawmux-tabs button {{ background: transparent; color: {dim}; border: none; box-shadow: none; border-radius: 6px; padding: 2px 12px; min-height: 0; }}
.clawmux-tabs button:hover {{ background: alpha({raised}, 0.7); color: {fg}; }}
.clawmux-tabs button.focused {{ background: {raised}; color: {fg}; box-shadow: inset 0 -2px 0 {accent}; }}
.clawmux-banner {{ background: {raised}; color: {fg}; padding: 6px 12px; }}
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
