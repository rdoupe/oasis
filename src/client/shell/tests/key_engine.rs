use super::*;
use crate::client::keys::{request_for_action, ActionContext, KeyEngine, KeyMode, KeyOutcome};
use crate::input::{KeybindAction, TerminalKey};
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

fn two_workspace_snapshot() -> ClientShellSnapshot {
    let mut snapshot = snapshot();
    let mut second = snapshot.workspaces[0].clone();
    second.workspace_id = "ws_2".into();
    second.number = 2;
    second.focused = false;
    snapshot.workspaces.push(second);
    let mut tab = snapshot.tabs[0].clone();
    tab.tab_id = "tab_2".into();
    tab.number = 2;
    tab.focused = false;
    snapshot.tabs.push(tab);
    snapshot
}

fn ctx<'a>(snapshot: &'a ClientShellSnapshot, order: &'a [usize]) -> ActionContext<'a> {
    ActionContext {
        snapshot,
        agent_order: &[],
        workspace_order: order,
        previous_pane_id: None,
        prompt_new_tab_name: false,
    }
}

fn engine() -> KeyEngine {
    KeyEngine::new(ClientShellConfig::from_config(&Config::default()).keybinds)
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> TerminalKey {
    TerminalKey::new(code, modifiers)
}

#[test]
fn key_engine_prefix_then_binding_sends_the_request() {
    let snapshot = snapshot();
    let mut engine = engine();
    let prefix = engine.key(
        key(KeyCode::Char('b'), KeyModifiers::CONTROL),
        &ctx(&snapshot, &[0]),
    );
    assert!(matches!(
        prefix.as_slice(),
        [KeyOutcome::ModeChanged(KeyMode::Prefix)]
    ));
    let split = engine.key(
        key(KeyCode::Char('v'), KeyModifiers::NONE),
        &ctx(&snapshot, &[0]),
    );
    assert!(matches!(
        split.as_slice(),
        [KeyOutcome::ModeChanged(KeyMode::Terminal), KeyOutcome::Request(request)]
            if matches!(request.method, crate::api::schema::Method::PaneSplit(_))
    ));
}

#[test]
fn key_engine_forwards_plain_keys_and_only_their_releases() {
    let snapshot = snapshot();
    let mut engine = engine();
    let press = key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(matches!(
        engine.key(press.clone(), &ctx(&snapshot, &[0])).as_slice(),
        [KeyOutcome::ToPane(_)]
    ));
    let release = press.with_kind(KeyEventKind::Release);
    assert!(matches!(
        engine
            .key(release.clone(), &ctx(&snapshot, &[0]))
            .as_slice(),
        [KeyOutcome::ToPane(_)]
    ));
    assert!(engine.key(release, &ctx(&snapshot, &[0])).is_empty());
}

#[test]
fn key_engine_prefix_twice_sends_the_prefix_to_the_pane() {
    let snapshot = snapshot();
    let mut engine = engine();
    let prefix = key(KeyCode::Char('b'), KeyModifiers::CONTROL);
    engine.key(prefix.clone(), &ctx(&snapshot, &[0]));
    assert!(matches!(
        engine.key(prefix, &ctx(&snapshot, &[0])).as_slice(),
        [
            KeyOutcome::ModeChanged(KeyMode::Terminal),
            KeyOutcome::ToPane(_)
        ]
    ));
}

#[test]
fn key_engine_reports_client_side_actions() {
    let snapshot = snapshot();
    let mut engine = engine();
    engine.key(
        key(KeyCode::Char('b'), KeyModifiers::CONTROL),
        &ctx(&snapshot, &[0]),
    );
    let outcomes = engine.key(
        key(KeyCode::Char('?'), KeyModifiers::SHIFT),
        &ctx(&snapshot, &[0]),
    );
    assert!(outcomes
        .iter()
        .any(|outcome| matches!(outcome, KeyOutcome::Action(KeybindAction::Help))));
}

#[test]
fn action_requests_follow_sidebar_and_tab_order() {
    let snapshot = two_workspace_snapshot();
    let order = [1, 0];
    let next = request_for_action(KeybindAction::NextWorkspace, &ctx(&snapshot, &order))
        .expect("next workspace");
    assert!(matches!(
        &next.method,
        crate::api::schema::Method::WorkspaceFocus(target) if target.workspace_id == "ws_2"
    ));
    assert_eq!(next.reveal_workspace_id.as_deref(), Some("ws_2"));
    let tab =
        request_for_action(KeybindAction::NextTab, &ctx(&snapshot, &order)).expect("next tab");
    assert!(matches!(
        &tab.method,
        crate::api::schema::Method::TabFocus(target) if target.tab_id == "tab_2"
    ));
    assert!(request_for_action(KeybindAction::Help, &ctx(&snapshot, &order)).is_none());
}
