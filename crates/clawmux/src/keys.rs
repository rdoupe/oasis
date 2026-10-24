//! GTK key events translated into oasis's key types: crossterm keys for the keybind
//! matcher, and semantic pane input for the server, which encodes them for each app.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gtk::gdk;
use oasis_core::embed::keys::TerminalKey;
use oasis_core::embed::wire::{ClientKeyCode, ClientKeyKind, ClientPaneInputEvent};

pub struct Key {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub text: Option<String>,
}

impl Key {
    pub fn from_gdk(keyval: gdk::Key, state: gdk::ModifierType) -> Option<Self> {
        let mut modifiers = KeyModifiers::empty();
        if state.contains(gdk::ModifierType::SHIFT_MASK) {
            modifiers |= KeyModifiers::SHIFT;
        }
        if state.contains(gdk::ModifierType::CONTROL_MASK) {
            modifiers |= KeyModifiers::CONTROL;
        }
        if state.contains(gdk::ModifierType::ALT_MASK) {
            modifiers |= KeyModifiers::ALT;
        }
        if state.contains(gdk::ModifierType::SUPER_MASK) {
            modifiers |= KeyModifiers::SUPER;
        }
        let code = match keyval {
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => KeyCode::Enter,
            gdk::Key::Tab | gdk::Key::KP_Tab => KeyCode::Tab,
            gdk::Key::ISO_Left_Tab => KeyCode::BackTab,
            gdk::Key::BackSpace => KeyCode::Backspace,
            gdk::Key::Escape => KeyCode::Esc,
            gdk::Key::Left | gdk::Key::KP_Left => KeyCode::Left,
            gdk::Key::Right | gdk::Key::KP_Right => KeyCode::Right,
            gdk::Key::Up | gdk::Key::KP_Up => KeyCode::Up,
            gdk::Key::Down | gdk::Key::KP_Down => KeyCode::Down,
            gdk::Key::Home | gdk::Key::KP_Home => KeyCode::Home,
            gdk::Key::End | gdk::Key::KP_End => KeyCode::End,
            gdk::Key::Page_Up | gdk::Key::KP_Page_Up => KeyCode::PageUp,
            gdk::Key::Page_Down | gdk::Key::KP_Page_Down => KeyCode::PageDown,
            gdk::Key::Insert | gdk::Key::KP_Insert => KeyCode::Insert,
            gdk::Key::Delete | gdk::Key::KP_Delete => KeyCode::Delete,
            key => {
                if let Some(n) = function_key(key) {
                    KeyCode::F(n)
                } else {
                    let c = key.to_unicode()?;
                    if c.is_control() {
                        return None;
                    }
                    KeyCode::Char(c)
                }
            }
        };
        let text = match code {
            KeyCode::Char(c)
                if !modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                Some(c.to_string())
            }
            _ => None,
        };
        Some(Self {
            code,
            modifiers,
            text,
        })
    }

    pub fn terminal_key(&self) -> TerminalKey {
        TerminalKey::from(KeyEvent::new(self.code, self.modifiers))
    }

    pub fn is(&self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        self.code == code && self.modifiers == modifiers
    }

    pub fn pane_event(&self) -> Option<ClientPaneInputEvent> {
        let code = match self.code {
            KeyCode::Backspace => ClientKeyCode::Backspace,
            KeyCode::Enter => ClientKeyCode::Enter,
            KeyCode::Left => ClientKeyCode::Left,
            KeyCode::Right => ClientKeyCode::Right,
            KeyCode::Up => ClientKeyCode::Up,
            KeyCode::Down => ClientKeyCode::Down,
            KeyCode::Home => ClientKeyCode::Home,
            KeyCode::End => ClientKeyCode::End,
            KeyCode::PageUp => ClientKeyCode::PageUp,
            KeyCode::PageDown => ClientKeyCode::PageDown,
            KeyCode::Tab => ClientKeyCode::Tab,
            KeyCode::BackTab => ClientKeyCode::BackTab,
            KeyCode::Delete => ClientKeyCode::Delete,
            KeyCode::Insert => ClientKeyCode::Insert,
            KeyCode::Esc => ClientKeyCode::Esc,
            KeyCode::F(n) => ClientKeyCode::F(n),
            KeyCode::Char(c) => ClientKeyCode::Char(c),
            _ => return None,
        };
        let shifted_codepoint = match self.code {
            KeyCode::Char(c) if self.modifiers.contains(KeyModifiers::SHIFT) => Some(c as u32),
            _ => None,
        };
        Some(ClientPaneInputEvent::Key {
            code,
            modifiers: self.modifiers.bits(),
            kind: ClientKeyKind::Press,
            repeat_count: 1,
            shifted_codepoint,
            generated_text: self.text.clone(),
            tracks_release: false,
            physical_key_id: None,
            windows_record: None,
        })
    }
}

fn function_key(key: gdk::Key) -> Option<u8> {
    use gtk::glib::translate::IntoGlib as _;
    let f1 = gdk::Key::F1.into_glib();
    let raw = key.into_glib();
    (f1..f1 + 24).contains(&raw).then(|| (raw - f1 + 1) as u8)
}

/// Whether a keyval is a bare modifier press, which never reaches panes or bindings.
pub fn is_modifier(keyval: gdk::Key) -> bool {
    matches!(
        keyval,
        gdk::Key::Shift_L
            | gdk::Key::Shift_R
            | gdk::Key::Control_L
            | gdk::Key::Control_R
            | gdk::Key::Alt_L
            | gdk::Key::Alt_R
            | gdk::Key::Super_L
            | gdk::Key::Super_R
            | gdk::Key::Meta_L
            | gdk::Key::Meta_R
            | gdk::Key::Caps_Lock
            | gdk::Key::ISO_Level3_Shift
    )
}
