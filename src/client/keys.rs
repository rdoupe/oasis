//! The key engine shared by the TUI and embedders.
//!
//! [`route_key`] is the mode and prefix state machine: it decides whether a
//! key press goes to the focused pane, runs a binding, or belongs to the
//! current mode's own handler. [`request_for_action`] maps a bound action to
//! the server method it sends. Both are pure, so the TUI's
//! `ClientShellState` and [`KeyEngine`] produce identical results.

use crossterm::event::{KeyCode, KeyEventKind};

use crate::api::schema::{
    Method, PaneDirection, PaneFocusDirectionParams, PaneResizeParams, PaneSplitParams,
    PaneSwapParams, PaneTarget, PaneZoomMode, PaneZoomParams, SplitDirection, TabCreateParams,
    TabMoveParams, TabTarget, WorkspaceTarget,
};
use crate::config::{CustomCommandKeybind, LiveKeybindConfig};
use crate::input::{KeyIdentity, KeybindAction, KeybindMatch, TerminalKey};
use crate::protocol::ClientShellSnapshot;

/// The client's input mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyMode {
    /// Keys go to the focused pane unless they match a direct binding.
    Terminal,
    /// The prefix was pressed; the next key picks a prefix binding.
    Prefix,
    /// Keyboard workspace and pane navigation. Its keys depend on sidebar
    /// layout, so they are reported as [`KeyStep::ModeKey`].
    Navigate,
    /// Pane resizing with h/j/k/l or the arrows.
    Resize,
    /// Copy mode on the focused pane. Its keys are reported as
    /// [`KeyStep::ModeKey`], except the prefix.
    Copy,
}

/// What a key press does after mode routing.
#[derive(Clone, Debug)]
pub enum KeyStep {
    /// Forward the key to the focused pane.
    ToPane,
    /// Run this binding.
    Binding(KeybindMatch),
    /// The key changed mode or was swallowed; nothing else happens.
    Consumed,
    /// The key belongs to the current mode's own handler (navigate or copy).
    ModeKey,
}

/// The result of routing one key press.
#[derive(Clone, Debug)]
pub struct KeyTransition {
    /// The mode after the key.
    pub mode: KeyMode,
    pub step: KeyStep,
}

/// Routes one key press through the mode and prefix state machine.
///
/// `copy_active` says whether copy mode is open on the focused pane, which is
/// where the prefix and resize modes return to. `copy_search_open` says
/// whether copy mode's search prompt has the keyboard, which takes the prefix
/// key as text.
pub fn route_key(
    config: &LiveKeybindConfig,
    mode: KeyMode,
    copy_active: bool,
    copy_search_open: bool,
    key: &TerminalKey,
) -> KeyTransition {
    let return_mode = if copy_active {
        KeyMode::Copy
    } else {
        KeyMode::Terminal
    };
    let to = |mode, step| KeyTransition { mode, step };
    match mode {
        KeyMode::Terminal => {
            if let Some(binding) = crate::input::resolve_direct_binding(&config.keybinds, key) {
                return to(mode, KeyStep::Binding(binding));
            }
            if config.matches_prefix(key) {
                return to(KeyMode::Prefix, KeyStep::Consumed);
            }
            to(mode, KeyStep::ToPane)
        }
        KeyMode::Prefix => {
            if config.matches_prefix(key) {
                return to(return_mode, KeyStep::ToPane);
            }
            if key.code == KeyCode::Esc {
                return to(return_mode, KeyStep::Consumed);
            }
            match crate::input::resolve_prefix_binding(&config.keybinds, key) {
                Some(binding) => to(return_mode, KeyStep::Binding(binding)),
                None => to(return_mode, KeyStep::Consumed),
            }
        }
        KeyMode::Navigate => to(mode, KeyStep::ModeKey),
        KeyMode::Resize => {
            let resize_bindings = &config.keybinds.resize_mode;
            if key.code == KeyCode::Esc
                || key.code == KeyCode::Enter
                || resize_bindings.matches_prefix_key(key)
                || resize_bindings.matches_direct_key(key)
            {
                return to(return_mode, KeyStep::Consumed);
            }
            let action = match key.code {
                KeyCode::Char('h') | KeyCode::Left => Some(KeybindAction::ResizePaneLeft),
                KeyCode::Char('j') | KeyCode::Down => Some(KeybindAction::ResizePaneDown),
                KeyCode::Char('k') | KeyCode::Up => Some(KeybindAction::ResizePaneUp),
                KeyCode::Char('l') | KeyCode::Right => Some(KeybindAction::ResizePaneRight),
                _ => None,
            };
            match action {
                Some(action) => to(mode, KeyStep::Binding(KeybindMatch::Action(action))),
                None => to(mode, KeyStep::Consumed),
            }
        }
        KeyMode::Copy => {
            if !copy_search_open && config.matches_prefix(key) {
                to(KeyMode::Prefix, KeyStep::Consumed)
            } else {
                to(mode, KeyStep::ModeKey)
            }
        }
    }
}

/// What [`request_for_action`] needs to know about the client's view.
pub struct ActionContext<'a> {
    pub snapshot: &'a ClientShellSnapshot,
    /// Agent pane ids in sidebar order, for the agent-focus actions.
    pub agent_order: &'a [String],
    /// Indexes into `snapshot.workspaces` in sidebar order (worktree groups
    /// collapsed as shown), for the workspace actions.
    pub workspace_order: &'a [usize],
    /// The pane focused before the current one, for `LastPane`.
    pub previous_pane_id: Option<&'a str>,
    /// When set, `NewTab` asks for a name instead of creating a tab directly.
    pub prompt_new_tab_name: bool,
}

/// A server request produced by a bound action.
#[derive(Clone, Debug)]
pub struct ActionRequest {
    pub method: Method,
    /// A workspace the client should scroll into view in its sidebar.
    pub reveal_workspace_id: Option<String>,
    /// The position in `agent_order` of an agent being focused, so the
    /// client can scroll its agent list to it.
    pub agent_index: Option<usize>,
}

impl ActionRequest {
    fn method(method: Method) -> Self {
        Self {
            method,
            reveal_workspace_id: None,
            agent_index: None,
        }
    }
}

/// The server method a bound action sends, or `None` when the action is
/// handled by the client (overlays, sidebar, copy mode) or has no target.
pub fn request_for_action(action: KeybindAction, ctx: &ActionContext<'_>) -> Option<ActionRequest> {
    let snapshot = ctx.snapshot;
    let focused_workspace = snapshot.focused_workspace_id.clone()?;
    let focused_tab = snapshot.focused_tab_id.clone();
    let focused_pane = snapshot.focused_pane_id.clone();
    let direction = |action| match action {
        KeybindAction::FocusPaneLeft
        | KeybindAction::SwapPaneLeft
        | KeybindAction::ResizePaneLeft => Some(PaneDirection::Left),
        KeybindAction::FocusPaneDown
        | KeybindAction::SwapPaneDown
        | KeybindAction::ResizePaneDown => Some(PaneDirection::Down),
        KeybindAction::FocusPaneUp | KeybindAction::SwapPaneUp | KeybindAction::ResizePaneUp => {
            Some(PaneDirection::Up)
        }
        KeybindAction::FocusPaneRight
        | KeybindAction::SwapPaneRight
        | KeybindAction::ResizePaneRight => Some(PaneDirection::Right),
        _ => None,
    };

    match action {
        KeybindAction::FocusAgent(index) => {
            Some(ActionRequest::method(Method::PaneFocus(PaneTarget {
                pane_id: ctx.agent_order.get(index)?.clone(),
            })))
        }
        KeybindAction::PreviousAgent | KeybindAction::NextAgent => {
            let agents = ctx.agent_order;
            if agents.is_empty() {
                return None;
            }
            let current = agents
                .iter()
                .position(|pane_id| Some(pane_id.as_str()) == snapshot.focused_pane_id.as_deref());
            let next = match (current, action) {
                (Some(current), KeybindAction::PreviousAgent) => {
                    (current + agents.len() - 1) % agents.len()
                }
                (Some(current), _) => (current + 1) % agents.len(),
                (None, KeybindAction::PreviousAgent) => agents.len() - 1,
                (None, _) => 0,
            };
            Some(ActionRequest {
                agent_index: Some(next),
                ..ActionRequest::method(Method::PaneFocus(PaneTarget {
                    pane_id: agents[next].clone(),
                }))
            })
        }
        KeybindAction::SwitchWorkspace(index) => {
            let workspace_id = snapshot
                .workspaces
                .get(*ctx.workspace_order.get(index)?)?
                .workspace_id
                .clone();
            Some(ActionRequest {
                reveal_workspace_id: Some(workspace_id.clone()),
                ..ActionRequest::method(Method::WorkspaceFocus(WorkspaceTarget { workspace_id }))
            })
        }
        KeybindAction::PreviousWorkspace | KeybindAction::NextWorkspace => {
            let entries = ctx.workspace_order;
            if entries.is_empty() {
                return None;
            }
            let current = entries
                .iter()
                .position(|&index| snapshot.workspaces[index].workspace_id == focused_workspace)
                .unwrap_or(0);
            let delta = if action == KeybindAction::PreviousWorkspace {
                -1
            } else {
                1
            };
            let next = (current as isize + delta).rem_euclid(entries.len() as isize) as usize;
            let workspace_id = snapshot.workspaces[entries[next]].workspace_id.clone();
            Some(ActionRequest {
                reveal_workspace_id: Some(workspace_id.clone()),
                ..ActionRequest::method(Method::WorkspaceFocus(WorkspaceTarget { workspace_id }))
            })
        }
        KeybindAction::SwitchTab(index) => {
            let tabs = snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == focused_workspace)
                .collect::<Vec<_>>();
            Some(ActionRequest::method(Method::TabFocus(TabTarget {
                tab_id: tabs.get(index)?.tab_id.clone(),
            })))
        }
        KeybindAction::PreviousTab | KeybindAction::NextTab => {
            let tabs = snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == focused_workspace)
                .collect::<Vec<_>>();
            let focused_tab = focused_tab?;
            let current = tabs.iter().position(|tab| tab.tab_id == focused_tab)?;
            let delta = if action == KeybindAction::PreviousTab {
                -1
            } else {
                1
            };
            let next = (current as isize + delta).rem_euclid(tabs.len() as isize) as usize;
            Some(ActionRequest::method(Method::TabFocus(TabTarget {
                tab_id: tabs[next].tab_id.clone(),
            })))
        }
        KeybindAction::MoveTabPrevious | KeybindAction::MoveTabNext => {
            let tabs = snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == focused_workspace)
                .collect::<Vec<_>>();
            if tabs.len() <= 1 {
                return None;
            }
            let focused_tab = focused_tab?;
            let source = tabs.iter().position(|tab| tab.tab_id == focused_tab)?;
            let insert_index = if action == KeybindAction::MoveTabNext {
                if source + 1 >= tabs.len() {
                    0
                } else {
                    source + 2
                }
            } else if source == 0 {
                tabs.len()
            } else {
                source - 1
            };
            Some(ActionRequest::method(Method::TabMove(TabMoveParams {
                tab_id: focused_tab,
                insert_index,
            })))
        }
        KeybindAction::NewTab if !ctx.prompt_new_tab_name => {
            Some(ActionRequest::method(Method::TabCreate(TabCreateParams {
                workspace_id: Some(focused_workspace),
                cwd: None,
                focus: true,
                label: None,
                env: Default::default(),
            })))
        }
        KeybindAction::FocusPaneLeft
        | KeybindAction::FocusPaneDown
        | KeybindAction::FocusPaneUp
        | KeybindAction::FocusPaneRight => Some(ActionRequest::method(Method::PaneFocusDirection(
            PaneFocusDirectionParams {
                pane_id: focused_pane,
                direction: direction(action)?,
            },
        ))),
        KeybindAction::SwapPaneLeft
        | KeybindAction::SwapPaneDown
        | KeybindAction::SwapPaneUp
        | KeybindAction::SwapPaneRight => {
            Some(ActionRequest::method(Method::PaneSwap(PaneSwapParams {
                pane_id: focused_pane,
                direction: Some(direction(action)?),
                source_pane_id: None,
                target_pane_id: None,
            })))
        }
        KeybindAction::SplitVertical | KeybindAction::SplitHorizontal => {
            Some(ActionRequest::method(Method::PaneSplit(PaneSplitParams {
                workspace_id: Some(focused_workspace),
                target_pane_id: focused_pane,
                direction: if action == KeybindAction::SplitVertical {
                    SplitDirection::Right
                } else {
                    SplitDirection::Down
                },
                ratio: None,
                cwd: None,
                focus: true,
                right_click: Default::default(),
                env: Default::default(),
            })))
        }
        KeybindAction::CyclePaneNext | KeybindAction::CyclePanePrevious => {
            let focused_tab = focused_tab?;
            let panes = snapshot
                .panes
                .iter()
                .filter(|pane| pane.tab_id == focused_tab)
                .collect::<Vec<_>>();
            if panes.is_empty() {
                return None;
            }
            let focused_pane = focused_pane?;
            let current = panes
                .iter()
                .position(|pane| pane.pane_id == focused_pane)
                .unwrap_or(0);
            let next = if action == KeybindAction::CyclePanePrevious {
                (current + panes.len() - 1) % panes.len()
            } else {
                (current + 1) % panes.len()
            };
            Some(ActionRequest::method(Method::PaneFocus(PaneTarget {
                pane_id: panes[next].pane_id.clone(),
            })))
        }
        KeybindAction::LastPane => {
            let pane_id = ctx.previous_pane_id?;
            if Some(pane_id) == focused_pane.as_deref()
                || !snapshot.panes.iter().any(|pane| pane.pane_id == pane_id)
            {
                return None;
            }
            Some(ActionRequest::method(Method::PaneFocus(PaneTarget {
                pane_id: pane_id.to_owned(),
            })))
        }
        KeybindAction::Zoom => Some(ActionRequest::method(Method::PaneZoom(PaneZoomParams {
            pane_id: focused_pane,
            mode: PaneZoomMode::Toggle,
        }))),
        KeybindAction::ClearPane => Some(ActionRequest::method(Method::PaneClear(PaneTarget {
            pane_id: focused_pane?,
        }))),
        KeybindAction::EditScrollback => Some(ActionRequest::method(Method::PaneEditScrollback(
            PaneTarget {
                pane_id: focused_pane?,
            },
        ))),
        KeybindAction::ResizePaneLeft
        | KeybindAction::ResizePaneDown
        | KeybindAction::ResizePaneUp
        | KeybindAction::ResizePaneRight => Some(ActionRequest::method(Method::PaneResize(
            PaneResizeParams {
                pane_id: focused_pane,
                direction: direction(action)?,
                amount: None,
            },
        ))),
        _ => None,
    }
}

/// What a key does, in order, for an embedder.
#[derive(Clone, Debug)]
pub enum KeyOutcome {
    /// Send this key to the focused pane (convert with
    /// `PaneInput::from_terminal_key`).
    ToPane(TerminalKey),
    /// Send this request to the server.
    Request(Box<ActionRequest>),
    /// A client-side action with no server method: overlays (help, settings,
    /// rename and close prompts, the navigator), the sidebar toggle, copy
    /// mode, detach, reload. The embedder handles it in its own UI.
    Action(KeybindAction),
    /// A custom command binding.
    Command(CustomCommandKeybind),
    /// The mode changed.
    ModeChanged(KeyMode),
    /// A key for the current mode's own handler (navigate or copy mode).
    ModeKey(KeyMode, TerminalKey),
}

/// Herdr's key handling for embedders: modes, the prefix, bindings and the
/// action-to-request mapping, with key releases routed where their press went.
pub struct KeyEngine {
    config: LiveKeybindConfig,
    mode: KeyMode,
    copy_active: bool,
    copy_search_open: bool,
    /// Keys whose press went to the pane; only their releases follow.
    forwarded: Vec<KeyIdentity>,
}

impl KeyEngine {
    pub fn new(config: LiveKeybindConfig) -> Self {
        Self {
            config,
            mode: KeyMode::Terminal,
            copy_active: false,
            copy_search_open: false,
            forwarded: Vec::new(),
        }
    }

    pub fn config(&self) -> &LiveKeybindConfig {
        &self.config
    }

    pub fn set_config(&mut self, config: LiveKeybindConfig) {
        self.config = config;
    }

    pub fn mode(&self) -> KeyMode {
        self.mode
    }

    /// Sets the mode, e.g. when the embedder opens navigate, resize or copy
    /// mode for a [`KeyOutcome::Action`].
    pub fn set_mode(&mut self, mode: KeyMode) {
        self.mode = mode;
    }

    /// Tells the engine whether copy mode is open on the focused pane, and
    /// whether its search prompt has the keyboard.
    pub fn set_copy_state(&mut self, active: bool, search_open: bool) {
        self.copy_active = active;
        self.copy_search_open = search_open;
    }

    /// Handles one key event and returns what it does, in order.
    pub fn key(&mut self, key: TerminalKey, ctx: &ActionContext<'_>) -> Vec<KeyOutcome> {
        let identity = key.identity();
        match key.kind {
            KeyEventKind::Release => {
                if let Some(index) = self.forwarded.iter().position(|held| *held == identity) {
                    self.forwarded.swap_remove(index);
                    return vec![KeyOutcome::ToPane(key)];
                }
                return Vec::new();
            }
            KeyEventKind::Repeat if self.forwarded.contains(&identity) => {
                return vec![KeyOutcome::ToPane(key)];
            }
            KeyEventKind::Press | KeyEventKind::Repeat => {}
        }
        if matches!(key.code, KeyCode::Modifier(_)) {
            return Vec::new();
        }
        let before = self.mode;
        let transition = route_key(
            &self.config,
            self.mode,
            self.copy_active,
            self.copy_search_open,
            &key,
        );
        self.mode = transition.mode;
        let mut outcomes = Vec::new();
        if self.mode != before {
            outcomes.push(KeyOutcome::ModeChanged(self.mode));
        }
        match transition.step {
            KeyStep::ToPane => {
                if key.kind == KeyEventKind::Press && !self.forwarded.contains(&identity) {
                    self.forwarded.push(identity);
                }
                outcomes.push(KeyOutcome::ToPane(key));
            }
            KeyStep::Binding(KeybindMatch::Command(command)) => {
                outcomes.push(KeyOutcome::Command(command));
            }
            KeyStep::Binding(KeybindMatch::Action(action)) => {
                match request_for_action(action, ctx) {
                    Some(request) => outcomes.push(KeyOutcome::Request(Box::new(request))),
                    None => outcomes.push(KeyOutcome::Action(action)),
                }
            }
            KeyStep::Consumed => {}
            KeyStep::ModeKey => outcomes.push(KeyOutcome::ModeKey(self.mode, key)),
        }
        outcomes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn config() -> LiveKeybindConfig {
        let config = crate::config::Config::default();
        LiveKeybindConfig {
            prefix: config.prefix_keys(),
            keybinds: config.keybinds(),
        }
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> TerminalKey {
        TerminalKey::new(code, modifiers)
    }

    #[test]
    fn resize_mode_maps_hjkl_and_exits_on_escape() {
        let config = config();
        let step = route_key(
            &config,
            KeyMode::Resize,
            false,
            false,
            &key(KeyCode::Char('h'), KeyModifiers::NONE),
        );
        assert_eq!(step.mode, KeyMode::Resize);
        assert!(matches!(
            step.step,
            KeyStep::Binding(KeybindMatch::Action(KeybindAction::ResizePaneLeft))
        ));
        let exit = route_key(
            &config,
            KeyMode::Resize,
            true,
            false,
            &key(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert_eq!(exit.mode, KeyMode::Copy, "resize returns to open copy mode");
    }

    #[test]
    fn copy_mode_search_takes_the_prefix_as_text() {
        let config = config();
        let prefix = key(KeyCode::Char('b'), KeyModifiers::CONTROL);
        assert_eq!(
            route_key(&config, KeyMode::Copy, true, false, &prefix).mode,
            KeyMode::Prefix
        );
        assert!(matches!(
            route_key(&config, KeyMode::Copy, true, true, &prefix).step,
            KeyStep::ModeKey
        ));
    }

    #[test]
    fn terminal_prefix_escape_and_unbound_keys() {
        let config = config();
        let prefix = key(KeyCode::Char('b'), KeyModifiers::CONTROL);
        let entered = route_key(&config, KeyMode::Terminal, false, false, &prefix);
        assert_eq!(entered.mode, KeyMode::Prefix);
        assert!(matches!(entered.step, KeyStep::Consumed));
        let escaped = route_key(
            &config,
            KeyMode::Prefix,
            false,
            false,
            &key(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert_eq!(escaped.mode, KeyMode::Terminal);
        assert!(matches!(escaped.step, KeyStep::Consumed));
        let typed = route_key(
            &config,
            KeyMode::Terminal,
            false,
            false,
            &key(KeyCode::Char('a'), KeyModifiers::NONE),
        );
        assert!(matches!(typed.step, KeyStep::ToPane));
    }
}
