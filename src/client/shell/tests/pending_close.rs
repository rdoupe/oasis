//! Closes are shown at once and settled by the server's answer.

use super::*;
use crate::api::schema::{Method, ResponseResult};

/// One workspace with tabs alpha, beta (focused) and gamma, one pane each,
/// plus a second pane in beta when `split` is set.
fn three_tabs(split: bool) -> ClientShellState {
    let mut projected = snapshot();
    projected.tabs.clear();
    projected.panes.clear();
    for (number, label) in [(1, "alpha"), (2, "beta"), (3, "gamma")] {
        let mut tab = snapshot().tabs[0].clone();
        tab.tab_id = format!("tab_{number}");
        tab.number = number;
        tab.label = label.into();
        tab.focused = number == 2;
        projected.tabs.push(tab);
        let mut pane = snapshot().panes[0].clone();
        pane.pane_id = format!("pane_{number}");
        pane.tab_id = format!("tab_{number}");
        pane.focused = number == 2;
        projected.panes.push(pane);
    }
    if split {
        let mut pane = projected.panes[1].clone();
        pane.pane_id = "pane_2b".into();
        pane.focused = false;
        projected.panes.push(pane);
    }
    projected.workspaces[0].active_tab_id = "tab_2".into();
    projected.focused_tab_id = Some("tab_2".into());
    projected.focused_pane_id = Some("pane_2".into());
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.config.confirm_close = false;
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 24).unwrap();
    state
}

fn press(state: &mut ClientShellState, action: crate::input::KeybindAction) -> ClientShellInput {
    let mut outcome = ClientShellInput::default();
    state.record_binding(crate::input::KeybindMatch::Action(action), &mut outcome);
    outcome
}

fn close_request(outcome: &ClientShellInput) -> (String, String, Method) {
    outcome
        .actions
        .iter()
        .find_map(|action| match action {
            ClientShellAction::Endpoint {
                boot_id, request, ..
            } if matches!(
                request.method,
                Method::TabClose(_) | Method::PaneClose(_) | Method::WorkspaceClose(_)
            ) =>
            {
                Some((boot_id.clone(), request.id.clone(), request.method.clone()))
            }
            _ => None,
        })
        .expect("close request sent")
}

fn tab_labels(state: &ClientShellState) -> Vec<String> {
    state
        .snapshot
        .as_deref()
        .unwrap()
        .tabs
        .iter()
        .map(|tab| tab.label.clone())
        .collect()
}

fn server_snapshot_without_tab(state: &ClientShellState, tab_id: &str) -> ClientShellSnapshot {
    let mut next = state
        .unfiltered_snapshots
        .values()
        .next()
        .unwrap()
        .as_ref()
        .clone();
    next.revision += 1;
    let pane_ids = next
        .panes
        .iter()
        .filter(|pane| pane.tab_id == tab_id)
        .map(|pane| pane.pane_id.clone())
        .collect::<Vec<_>>();
    next.tabs.retain(|tab| tab.tab_id != tab_id);
    next.panes.retain(|pane| !pane_ids.contains(&pane.pane_id));
    next
}

fn error(code: &str) -> Result<ResponseResult, ClientShellEndpointError> {
    Err(ClientShellEndpointError {
        code: Some(code.into()),
        message: "could not close".into(),
    })
}

#[test]
fn closing_a_tab_hides_it_at_once_and_focuses_the_servers_neighbor() {
    let mut state = three_tabs(false);
    let closed = press(&mut state, crate::input::KeybindAction::CloseTab);
    let (_, _, method) = close_request(&closed);
    assert!(matches!(method, Method::TabClose(target) if target.tab_id == "tab_2"));
    assert!(closed.repaint);
    assert_eq!(tab_labels(&state), ["alpha", "gamma"]);
    let shown = state.snapshot.as_deref().unwrap();
    // Workspace::close_tab moves to the previous tab.
    assert_eq!(shown.focused_tab_id.as_deref(), Some("tab_1"));
    assert_eq!(shown.focused_pane_id.as_deref(), Some("pane_1"));
    assert_eq!(shown.workspaces[0].active_tab_id, "tab_1");
    assert_eq!(
        shown.tabs.iter().map(|tab| tab.number).collect::<Vec<_>>(),
        [1, 2]
    );
    let text = frame_rows(&state.compose(106, 24).unwrap()).join("\n");
    assert!(!text.contains("beta"), "{text}");

    // The next key acts on what is shown: closing again targets alpha.
    let again = press(&mut state, crate::input::KeybindAction::CloseTab);
    assert!(
        matches!(close_request(&again).2, Method::TabClose(target) if target.tab_id == "tab_1")
    );
}

#[test]
fn a_snapshot_without_the_tab_settles_the_close() {
    let mut state = three_tabs(false);
    let closed = press(&mut state, crate::input::KeybindAction::CloseTab);
    let (boot_id, request_id, _) = close_request(&closed);
    let next = server_snapshot_without_tab(&state, "tab_2");
    state.set_snapshot(Box::new(next));
    assert!(state.pending_closes.is_empty());
    assert!(state.unfiltered_snapshots.is_empty());
    assert_eq!(tab_labels(&state), ["alpha", "gamma"]);
    let (repaint, _) =
        state.handle_endpoint_result(&boot_id, &request_id, Ok(ResponseResult::Ok {}));
    assert!(!repaint);
    assert_eq!(tab_labels(&state), ["alpha", "gamma"]);
}

#[test]
fn a_failed_or_timed_out_close_puts_the_tab_back_with_a_notice() {
    for code in ["tab_close_failed", "endpoint_timeout"] {
        let mut state = three_tabs(false);
        let closed = press(&mut state, crate::input::KeybindAction::CloseTab);
        let (boot_id, request_id, _) = close_request(&closed);
        assert_eq!(tab_labels(&state), ["alpha", "gamma"]);
        let (repaint, _) = state.handle_endpoint_result(&boot_id, &request_id, error(code));
        assert!(repaint, "{code}");
        assert_eq!(tab_labels(&state), ["alpha", "beta", "gamma"], "{code}");
        assert_eq!(
            state.snapshot.as_deref().unwrap().focused_tab_id.as_deref(),
            Some("tab_2")
        );
        assert!(state.visible_endpoint_notice.is_some(), "{code}");
        assert!(state.pending_closes.is_empty());
    }
}

#[test]
fn a_stale_snapshot_during_the_close_does_not_bring_the_tab_back() {
    let mut state = three_tabs(false);
    let closed = press(&mut state, crate::input::KeybindAction::CloseTab);
    let (boot_id, request_id, _) = close_request(&closed);
    let mut stale = state
        .unfiltered_snapshots
        .values()
        .next()
        .unwrap()
        .as_ref()
        .clone();
    stale.revision += 1;
    state.set_snapshot(Box::new(stale));
    assert_eq!(tab_labels(&state), ["alpha", "gamma"]);
    assert_eq!(state.pending_closes.len(), 1);

    // Once the server confirms, the next snapshot is the truth again.
    state.handle_endpoint_result(&boot_id, &request_id, Ok(ResponseResult::Ok {}));
    assert_eq!(tab_labels(&state), ["alpha", "gamma"]);
    let next = server_snapshot_without_tab(&state, "tab_2");
    state.set_snapshot(Box::new(next));
    assert!(state.pending_closes.is_empty());
    assert_eq!(tab_labels(&state), ["alpha", "gamma"]);
}

#[test]
fn closing_a_pane_hides_it_and_focuses_a_sibling() {
    let mut state = three_tabs(true);
    let closed = press(&mut state, crate::input::KeybindAction::ClosePane);
    assert!(
        matches!(close_request(&closed).2, Method::PaneClose(target) if target.pane_id == "pane_2")
    );
    let shown = state.snapshot.as_deref().unwrap();
    assert!(!shown.panes.iter().any(|pane| pane.pane_id == "pane_2"));
    assert_eq!(shown.focused_pane_id.as_deref(), Some("pane_2b"));
    assert_eq!(shown.focused_tab_id.as_deref(), Some("tab_2"));
    assert_eq!(tab_labels(&state), ["alpha", "beta", "gamma"]);
}

#[test]
fn closing_a_tabs_last_pane_hides_the_tab() {
    let mut state = three_tabs(false);
    press(&mut state, crate::input::KeybindAction::ClosePane);
    assert_eq!(tab_labels(&state), ["alpha", "gamma"]);
}

#[test]
fn closing_a_workspace_hides_it_and_focuses_the_one_in_its_place() {
    let mut projected = snapshot();
    let mut second = projected.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.number = 2;
    second.label = "second".into();
    second.active_tab_id = "tab_w2".into();
    projected.workspaces[0].focused = false;
    projected.workspaces.push(second);
    let mut tab = projected.tabs[0].clone();
    tab.tab_id = "tab_w2".into();
    tab.workspace_id = "ws_2".into();
    projected.tabs[0].focused = false;
    projected.tabs.push(tab);
    let mut pane = projected.panes[0].clone();
    pane.pane_id = "pane_w2".into();
    pane.tab_id = "tab_w2".into();
    pane.workspace_id = "ws_2".into();
    projected.panes[0].focused = false;
    projected.panes.push(pane);
    projected.focused_workspace_id = Some("ws_2".into());
    projected.focused_tab_id = Some("tab_w2".into());
    projected.focused_pane_id = Some("pane_w2".into());
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.config.confirm_close = false;
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.compose(106, 24).unwrap();

    let closed = press(&mut state, crate::input::KeybindAction::CloseWorkspace);
    assert!(matches!(close_request(&closed).2,
        Method::WorkspaceClose(params) if params.workspace_id == "ws_2"));
    let shown = state.snapshot.as_deref().unwrap();
    assert_eq!(shown.workspaces.len(), 1);
    assert_eq!(shown.focused_workspace_id.as_deref(), Some("ws_1"));
    assert_eq!(shown.focused_tab_id.as_deref(), Some("tab_1"));
    assert_eq!(shown.focused_pane_id.as_deref(), Some("pane_1"));
    assert!(!shown.panes.iter().any(|pane| pane.pane_id == "pane_w2"));
}

#[test]
fn confirm_close_still_asks_before_anything_is_hidden() {
    let mut state = three_tabs(false);
    state.config.confirm_close = true;
    // A workspace's last tab asks first; leave one tab.
    let mut single = state.snapshot.as_deref().unwrap().clone();
    single.revision += 1;
    single.tabs.retain(|tab| tab.tab_id == "tab_2");
    single.panes.retain(|pane| pane.tab_id == "tab_2");
    state.set_snapshot(Box::new(single));
    let asked = press(&mut state, crate::input::KeybindAction::CloseTab);
    assert!(asked.actions.is_empty());
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::ConfirmClose(_))
    ));
    assert_eq!(tab_labels(&state), ["beta"]);
    assert!(state.pending_closes.is_empty());
    let accepted = state.handle_input_bytes(b"\r");
    close_request(&accepted);
    assert!(state.snapshot.as_deref().unwrap().workspaces.is_empty());
}

#[test]
fn an_endpoint_reset_puts_pending_closes_back() {
    let mut state = three_tabs(false);
    press(&mut state, crate::input::KeybindAction::CloseTab);
    state.abandon_pending_closes();
    assert!(state.pending_closes.is_empty());
    let cached = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id == state.active_endpoint_id)
        .and_then(|endpoint| endpoint.snapshot.as_deref())
        .unwrap();
    assert_eq!(cached.tabs.len(), 3);
}
