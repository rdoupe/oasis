//! Tool logos for agent rows, so each session shows which agent it runs. The SVGs are
//! Lobe Icons (MIT, see assets/logos); single-color marks take the theme's text color.

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::{gdk, glib};

use oasis_core::embed::sidebar::Agent;

use crate::theme::Rgb;

const CLAUDE: &str = include_str!("../assets/logos/claude-color.svg");
const OPENAI: &str = include_str!("../assets/logos/openai.svg");
const CURSOR: &str = include_str!("../assets/logos/cursor.svg");
const GROK: &str = include_str!("../assets/logos/grok.svg");
const GEMINI: &str = include_str!("../assets/logos/gemini-color.svg");

/// The logo for an agent tool as oasis identifies it.
fn source(agent: Agent) -> Option<&'static str> {
    match agent {
        Agent::Claude => Some(CLAUDE),
        Agent::Codex => Some(OPENAI),
        Agent::Cursor => Some(CURSOR),
        Agent::Grok => Some(GROK),
        Agent::Gemini => Some(GEMINI),
        _ => None,
    }
}

thread_local! {
    static CACHE: RefCell<HashMap<(usize, Rgb), Option<gdk::Texture>>> = RefCell::new(HashMap::new());
}

/// A texture of the agent's logo, drawn in `fg` where the mark has no colors of its own.
pub fn texture(agent: Agent, fg: Rgb) -> Option<gdk::Texture> {
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
    fn agent_tools_map_to_their_logos() {
        assert_eq!(source(Agent::Claude), Some(CLAUDE));
        assert_eq!(source(Agent::Codex), Some(OPENAI));
        assert_eq!(source(Agent::Cursor), Some(CURSOR));
        assert_eq!(source(Agent::Grok), Some(GROK));
        assert_eq!(source(Agent::Gemini), Some(GEMINI));
        assert_eq!(source(Agent::Pi), None);
    }
}
