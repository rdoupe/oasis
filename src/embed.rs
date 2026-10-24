//! Embeddable client pieces for native front ends.
//!
//! A front end such as a GTK window talks to Oasis and Herdr servers with the
//! same client-shell protocol the TUI uses, without a terminal. [`wire`] is the
//! frozen generation-1 protocol surface, [`cell`] decodes the cells inside
//! server surfaces, and [`key`] is the neutral key event the key engine takes.

pub use crate::client::core::{
    ClientCore, CoreEvent, CoreOptions, Drained, EndpointError, EndpointId, EndpointStatus,
    EndpointUpdate, MachineSource, Negotiation, RequestId, SavedMachine, SurfaceUpdate,
};
/// Semantic input for a pane, sent with [`ClientCore::pane_input`].
pub use crate::protocol::ClientPaneInputEvent as PaneInput;

/// Decoders for the colors and styles packed into [`wire::CellData`].
///
/// The packing is part of the generation-1 surface codec: these helpers only
/// read it and never change what is sent.
pub mod cell {
    /// A cell color as packed on the wire.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Color {
        /// The terminal's default color.
        Reset,
        /// One of the 16 named ANSI colors: 1 is black through 16 white, in
        /// ratatui order (black, red, green, yellow, blue, magenta, cyan, gray,
        /// dark gray, light red … light cyan, white).
        Named(u8),
        /// A 256-color palette index.
        Indexed(u8),
        Rgb(u8, u8, u8),
    }

    /// Decodes a packed cell color. Unknown encodings read as [`Color::Reset`].
    pub fn decode_color(packed: u32) -> Color {
        match packed >> 24 {
            0x00 => match packed & 0xFF {
                0 => Color::Reset,
                named @ 1..=16 => Color::Named(named as u8),
                _ => Color::Reset,
            },
            0x01 => Color::Indexed((packed & 0xFF) as u8),
            0x02 => Color::Rgb(
                ((packed >> 16) & 0xFF) as u8,
                ((packed >> 8) & 0xFF) as u8,
                (packed & 0xFF) as u8,
            ),
            _ => Color::Reset,
        }
    }

    /// Style bits of a cell's `modifier`.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Style(pub u16);

    impl Style {
        pub const BOLD: u16 = 1 << 0;
        pub const DIM: u16 = 1 << 1;
        pub const ITALIC: u16 = 1 << 2;
        pub const UNDERLINED: u16 = 1 << 3;
        pub const SLOW_BLINK: u16 = 1 << 4;
        pub const RAPID_BLINK: u16 = 1 << 5;
        pub const REVERSED: u16 = 1 << 6;
        pub const HIDDEN: u16 = 1 << 7;
        pub const CROSSED_OUT: u16 = 1 << 8;

        pub fn has(self, bit: u16) -> bool {
            self.0 & bit != 0
        }
    }

    /// The underline shape of an underlined cell.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum UnderlineStyle {
        Single,
        Double,
        Curly,
        Dotted,
        Dashed,
    }

    /// Splits a cell's `modifier` into its style bits and, when underlined,
    /// the underline shape carried in the top four bits.
    pub fn decode_style(modifier: u16) -> (Style, Option<UnderlineStyle>) {
        let style = Style(modifier & 0x0FFF);
        let underline = style.has(Style::UNDERLINED).then(|| {
            match crate::protocol::underline_style_from_modifier(modifier) {
                2 => UnderlineStyle::Double,
                3 => UnderlineStyle::Curly,
                4 => UnderlineStyle::Dotted,
                5 => UnderlineStyle::Dashed,
                _ => UnderlineStyle::Single,
            }
        });
        (style, underline)
    }
}

/// A key event without terminal types, for the key engine.
///
/// Pane input itself goes over [`wire::ClientPaneInputEvent`] (re-exported as
/// `PaneInput`); this type is what a front end hands the shared key engine.
pub mod key {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct KeyEvent {
        pub code: KeyCode,
        pub mods: Mods,
        pub kind: KeyKind,
        pub repeat_count: u16,
        /// The codepoint the key produces with Shift, when the platform reports it.
        pub shifted_codepoint: Option<u32>,
        /// Text the key generates under the active layout and input method.
        pub generated_text: Option<String>,
        /// Whether the host reports this key's release, so press and release pair.
        pub tracks_release: bool,
        /// A layout-independent physical key identity (a scan code), when known.
        pub physical_key_id: Option<u32>,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum KeyCode {
        Char(char),
        Enter,
        Tab,
        BackTab,
        Backspace,
        Esc,
        Left,
        Right,
        Up,
        Down,
        Home,
        End,
        PageUp,
        PageDown,
        Insert,
        Delete,
        F(u8),
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Mods {
        pub shift: bool,
        pub ctrl: bool,
        pub alt: bool,
        pub super_: bool,
        pub caps_lock: bool,
        pub num_lock: bool,
    }

    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum KeyKind {
        #[default]
        Press,
        Repeat,
        Release,
    }
}

/// The frozen generation-1 client-shell protocol, for front ends that speak
/// it directly. Everything here is part of the stable endpoint contract.
pub mod wire {
    pub use crate::api::schema::{AgentStatus, Method, Request, ResponseResult};
    pub use crate::protocol::endpoint::{
        EndpointClientHello, EndpointServerWelcome, BLOB_CODEC_V1, ENDPOINT_HELLO_KIND,
        ENDPOINT_PROTOCOL_GENERATION, ENDPOINT_SNAPSHOT_KIND, ENDPOINT_WELCOME_KIND,
        HEALTH_PING_KIND, HEALTH_PONG_KIND, INPUT_CODEC_V1, PRESENTATION_EFFECTS_READY_KIND,
        PRESENTATION_EFFECTS_SYNC_KIND, SNAPSHOT_CODEC_V1, SURFACE_CODEC_V1,
    };
    pub use crate::protocol::{
        read_message, write_message, CellData, ClientHostAppearance, ClientHostColor,
        ClientHostDefaultColorKind, ClientHostThemeUpdate, ClientKeyCode, ClientKeyKind,
        ClientMessage, ClientMouseButton, ClientMouseGeometry, ClientMouseKind,
        ClientMousePosition, ClientPaneInputEvent, ClientShellAgent, ClientShellPane,
        ClientShellSnapshot, ClientShellTab, ClientShellWorkspace, ClientSurfaceSize, CursorState,
        FrameData, FramingError, PaneSurfaceFrame, PaneSurfacePane, PaneSurfacePatch,
        PaneSurfacePatchRow, PaneSurfaceSplit, SemanticNotification, ServerMessage, SurfaceRect,
        MAX_FRAME_SIZE,
    };

    /// A connected local stream (a Unix socket, or a named pipe on Windows).
    pub type LocalStream = crate::ipc::LocalStream;

    /// The client-shell socket of the local server for the current session.
    pub fn client_socket_path() -> std::path::PathBuf {
        crate::server::socket_paths::client_socket_path()
    }

    pub fn connect_local_stream(path: &std::path::Path) -> std::io::Result<LocalStream> {
        crate::ipc::connect_local_stream(path)
    }

    /// Starts `exe server` detached, as the TUI does when no local server is
    /// running, and waits up to `timeout` for its client socket.
    pub fn start_local_server(
        exe: std::path::PathBuf,
        timeout: std::time::Duration,
    ) -> std::io::Result<()> {
        crate::server::autodetect::spawn_server_daemon_with(exe)?;
        crate::server::autodetect::wait_for_server_socket(&client_socket_path(), timeout)
    }
}

/// Herdr's key handling: [`keys::KeyEngine`] runs the modes, the prefix and
/// the bindings and maps actions to server requests, exactly as the TUI does.
/// The binding types and [`keys::load`] were the interim API before the
/// engine; prefer the engine. Matching works on [`keys::TerminalKey`], which a
/// front end builds from a crossterm `KeyEvent` with `TerminalKey::from`.
/// Sidebar data derived from snapshots, shared with the TUI sidebar.
///
/// Set [`super::CoreOptions::sidebar`] (or call
/// [`super::ClientCore::set_sidebar_prefs`]) and `drain()` returns a fresh
/// [`SidebarModel`] in `Drained::sidebar` only when something a sidebar
/// shows changed. Tokens carry text and a neutral style; draw them in order.
pub mod sidebar {
    pub use crate::client::model::{
        hide_closed_target, notification_delivery, AgentEntry, CloseSettle, CloseTarget,
        FooterEntry, GroupToggle, IncomingCloses, MachineEntry, NotificationDelivery,
        NotificationToast, PendingCloses, SidebarEndpoint, SidebarModel, SidebarPrefs, SpaceEntry,
        TabEntry, TokenRow,
    };
    pub use crate::config::{
        AgentPanelSortConfig, AgentsSidebarConfig, SidebarTokenColor, SidebarTokenStyle,
        SpacesSidebarConfig, ToastDelivery,
    };
    pub use crate::detect::Agent;
    pub use crate::ui::{ActivityKind, ActivityPart, ResolvedToken, ResolvedTokenKind};
}

pub mod keys {
    pub use crate::client::keys::{
        request_for_action, route_key, ActionContext, ActionRequest, KeyEngine, KeyMode,
        KeyOutcome, KeyStep, KeyTransition,
    };
    pub use crate::config::{
        ActionKeybinds, CustomCommandKeybind, IndexedKeybind, Keybinds, LiveKeybindConfig,
    };
    pub use crate::input::{KeybindAction, KeybindMatch, TerminalKey};

    /// The live keybindings from the oasis config, as the TUI loads them:
    /// invalid bindings fall back to the configured prefix and defaults.
    pub fn load() -> LiveKeybindConfig {
        let config = crate::config::Config::load().config;
        config
            .live_keybinds_with_diagnostics()
            .map(|(keybinds, _diagnostics)| keybinds)
            .unwrap_or_else(|_diagnostics| LiveKeybindConfig {
                prefix: config.prefix_keys(),
                keybinds: config.keybinds(),
            })
    }
}

/// Herdr's mouse rules for panes: focus, selection, double clicks, plain and
/// Ctrl link clicks, pass-through to apps that report the mouse, and wheel
/// scrolling. See [`mouse::MouseEngine`].
pub mod mouse {
    pub use crate::client::mouse_engine::{
        is_repeat_click, released_in_place, wheel_scroll_offset, ClickSpot, MouseEngine,
        MouseOutcome, PanePress, MULTI_CLICK_INTERVAL, PLAIN_LINK_CLICK_DELAY,
    };
    pub use crate::pane::ScrollMetrics;
}

#[cfg(test)]
mod tests {
    use super::cell::{decode_color, decode_style, Color, Style, UnderlineStyle};

    #[test]
    fn decode_color_inverts_the_wire_packing() {
        use ratatui::style::Color as Tui;
        let named = [
            Tui::Black,
            Tui::Red,
            Tui::Green,
            Tui::Yellow,
            Tui::Blue,
            Tui::Magenta,
            Tui::Cyan,
            Tui::Gray,
            Tui::DarkGray,
            Tui::LightRed,
            Tui::LightGreen,
            Tui::LightYellow,
            Tui::LightBlue,
            Tui::LightMagenta,
            Tui::LightCyan,
            Tui::White,
        ];
        assert_eq!(
            decode_color(crate::protocol::color_to_u32(Tui::Reset)),
            Color::Reset
        );
        for (index, color) in named.into_iter().enumerate() {
            assert_eq!(
                decode_color(crate::protocol::color_to_u32(color)),
                Color::Named(index as u8 + 1)
            );
        }
        assert_eq!(
            decode_color(crate::protocol::color_to_u32(Tui::Indexed(208))),
            Color::Indexed(208)
        );
        assert_eq!(
            decode_color(crate::protocol::color_to_u32(Tui::Rgb(1, 2, 3))),
            Color::Rgb(1, 2, 3)
        );
        assert_eq!(decode_color(0x7F00_0000), Color::Reset);
    }

    #[test]
    fn decode_style_reads_bits_and_underline_shape() {
        use ratatui::style::Modifier;
        let bold_italic = crate::protocol::modifier_to_u16(Modifier::BOLD | Modifier::ITALIC);
        let (style, underline) = decode_style(bold_italic);
        assert!(style.has(Style::BOLD) && style.has(Style::ITALIC) && !style.has(Style::DIM));
        assert_eq!(underline, None);

        let curly = crate::protocol::modifier_with_underline_style(Modifier::UNDERLINED, 3);
        let (style, underline) = decode_style(curly.bits());
        assert!(style.has(Style::UNDERLINED));
        assert_eq!(style.0 & 0xF000, 0, "the shape bits are not style bits");
        assert_eq!(underline, Some(UnderlineStyle::Curly));
    }

    #[test]
    fn keys_load_reads_the_isolated_oasis_config() {
        // nextest runs each test in its own process, so setting the variable is isolated.
        let dir = std::env::temp_dir().join(format!("oasis-embed-keys-{}", std::process::id()));
        let previous = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("HOME", dir.join("home"));
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        let config_dir = crate::config::config_dir();
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("config.toml"),
            "[keys]\nprefix = \"ctrl+space\"\n",
        )
        .unwrap();
        let keys = super::keys::load();
        match previous {
            Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(keys.primary_prefix_label(), "ctrl+space");
        let prefix = super::keys::TerminalKey::from(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char(' '),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        assert!(keys.matches_prefix(&prefix));
    }
}
