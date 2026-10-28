//! Agent quick actions: touch buttons for the keys a coding agent wants that an
//! on-screen keyboard makes awkward. Each sends the agent's own key through the same
//! pane input as a typed key.

use crossterm::event::{KeyCode, KeyModifiers};
use oasis_core::embed::sidebar::Agent;
use oasis_core::embed::wire::ClientPaneInputEvent;

use crate::keys::Key;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickAction {
    /// Claude Code's `task:background` (ctrl+b): moves a running command to the background.
    Background,
    /// Esc: stops the agent's turn.
    Interrupt,
    /// Claude Code's `chat:sendNow` (ctrl+x ctrl+s, or ctrl+enter on terminals with the
    /// kitty keyboard protocol): sends the waiting messages to the model now.
    SendNow,
    /// Shift+Tab: cycles the permission mode.
    CycleMode,
}

impl QuickAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Background => "Background",
            Self::Interrupt => "Interrupt",
            Self::SendNow => "Send now",
            Self::CycleMode => "⇧Tab mode",
        }
    }

    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Background => "Run the current command in the background (ctrl+b)",
            Self::Interrupt => "Stop the agent (esc)",
            Self::SendNow => "Send the waiting message now (ctrl+x ctrl+s)",
            Self::CycleMode => "Cycle the permission mode (shift+tab)",
        }
    }

    /// The keys to send, in order.
    pub fn keys(self) -> Vec<Key> {
        let ctrl = |c| Key {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            text: None,
        };
        match self {
            Self::Background => vec![ctrl('b')],
            Self::Interrupt => vec![Key {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::empty(),
                text: None,
            }],
            // The plain chord; ctrl+enter only reaches apps that turned on the kitty
            // keyboard protocol.
            Self::SendNow => vec![ctrl('x'), ctrl('s')],
            Self::CycleMode => vec![Key {
                code: KeyCode::BackTab,
                modifiers: KeyModifiers::SHIFT,
                text: None,
            }],
        }
    }

    pub fn events(self) -> Vec<ClientPaneInputEvent> {
        self.keys().iter().filter_map(Key::pane_event).collect()
    }
}

/// The quick actions for an agent: Claude Code gets its own keys; other agents share
/// Esc to interrupt and Shift+Tab, which most of them use for their mode.
pub fn for_agent(agent: Agent) -> Vec<QuickAction> {
    match agent {
        Agent::Claude => vec![
            QuickAction::Interrupt,
            QuickAction::Background,
            QuickAction::SendNow,
            QuickAction::CycleMode,
        ],
        _ => vec![QuickAction::Interrupt, QuickAction::CycleMode],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oasis_core::embed::wire::ClientKeyCode;

    fn codes(action: QuickAction) -> Vec<(ClientKeyCode, u8)> {
        action
            .events()
            .into_iter()
            .filter_map(|event| match event {
                ClientPaneInputEvent::Key {
                    code, modifiers, ..
                } => Some((code, modifiers)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn quick_actions_send_claude_codes_own_keys() {
        let ctrl = KeyModifiers::CONTROL.bits();
        assert_eq!(
            codes(QuickAction::Background),
            vec![(ClientKeyCode::Char('b'), ctrl)]
        );
        assert_eq!(
            codes(QuickAction::SendNow),
            vec![(ClientKeyCode::Char('x'), ctrl), (ClientKeyCode::Char('s'), ctrl)]
        );
        assert_eq!(codes(QuickAction::Interrupt), vec![(ClientKeyCode::Esc, 0)]);
        assert_eq!(
            codes(QuickAction::CycleMode),
            vec![(ClientKeyCode::BackTab, KeyModifiers::SHIFT.bits())]
        );
    }

    #[test]
    fn only_claude_code_gets_background_and_send_now() {
        assert!(for_agent(Agent::Claude).contains(&QuickAction::SendNow));
        assert!(for_agent(Agent::Claude).contains(&QuickAction::Background));
        let codex = for_agent(Agent::Codex);
        assert!(codex.contains(&QuickAction::Interrupt));
        assert!(!codex.contains(&QuickAction::SendNow));
        assert!(!codex.contains(&QuickAction::Background));
    }
}
