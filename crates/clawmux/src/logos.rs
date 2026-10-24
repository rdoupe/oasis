//! Tool logos for agent rows, so each session shows which agent it runs. The SVGs are
//! Lobe Icons (MIT, see assets/logos); single-color marks take the theme's text color.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::{gdk, glib};

use crate::theme::Rgb;

const CLAUDE: &str = include_str!("../assets/logos/claude-color.svg");
const OPENAI: &str = include_str!("../assets/logos/openai.svg");
const CURSOR: &str = include_str!("../assets/logos/cursor.svg");
const GROK: &str = include_str!("../assets/logos/grok.svg");
const GEMINI: &str = include_str!("../assets/logos/gemini-color.svg");

/// The logo for an agent identity as oasis reports it (`claude`, `codex`, `cursor-agent`, …).
fn source(agent: &str) -> Option<&'static str> {
    let agent = agent.to_ascii_lowercase();
    let has = |name: &str| agent.contains(name);
    if has("claude") {
        Some(CLAUDE)
    } else if has("codex") || has("openai") || has("gpt") {
        Some(OPENAI)
    } else if has("cursor") {
        Some(CURSOR)
    } else if has("grok") {
        Some(GROK)
    } else if has("gemini") {
        Some(GEMINI)
    } else {
        None
    }
}

thread_local! {
    static CACHE: RefCell<HashMap<(usize, Rgb), Option<gdk::Texture>>> = RefCell::new(HashMap::new());
}

/// A texture of the agent's logo, drawn in `fg` where the mark has no colors of its own.
pub fn texture(agent: &str, fg: Rgb) -> Option<gdk::Texture> {
    let svg = source(agent)?;
    let key = (svg.as_ptr() as usize, fg);
    CACHE.with(|cache| {
        cache
            .borrow_mut()
            .entry(key)
            .or_insert_with(|| {
                let color = fg.css();
                // Cursor's cube is shaded black; shade it in the text color instead, keeping
                // its gradients' transparency so the faces still read on dark themes.
                let svg = if std::ptr::eq(svg, CURSOR) {
                    svg.replace("\"#000\"", &format!("\"{color}\"")).replace(
                        "fill=\"#555\"",
                        &format!("fill=\"{color}\" fill-opacity=\"0.55\""),
                    )
                } else {
                    svg.to_owned()
                };
                let svg = svg
                    .replace("currentColor", &color)
                    .replace("fill=\"#000\"", &format!("fill=\"{color}\""))
                    // Render at a size that stays crisp on HiDPI; the widget scales it down.
                    .replace("height=\"1em\"", "height=\"64\"")
                    .replace("width=\"1em\"", "width=\"64\"");
                gdk::Texture::from_bytes(&glib::Bytes::from_owned(svg.into_bytes())).ok()
            })
            .clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_identities_map_to_their_logos() {
        assert_eq!(source("claude"), Some(CLAUDE));
        assert_eq!(source("Claude Code"), Some(CLAUDE));
        assert_eq!(source("codex"), Some(OPENAI));
        assert_eq!(source("cursor-agent"), Some(CURSOR));
        assert_eq!(source("grok"), Some(GROK));
        assert_eq!(source("gemini"), Some(GEMINI));
        assert_eq!(source("pi"), None);
    }
}
