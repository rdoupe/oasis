use super::*;
use crate::herdr_import::{Availability, HerdrInstall, ImportDirs, ImportItem};

fn scratch_root(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oasis-shell-import-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn install(root: &std::path::Path, server: Availability) -> HerdrInstall {
    HerdrInstall {
        dirs: ImportDirs {
            oasis_config: root.join("config/oasis"),
            oasis_state: root.join("state/oasis"),
            herdr_config: root.join("config/herdr"),
            herdr_state: root.join("state/herdr"),
        },
        settings: Availability::Available,
        remotes: Availability::Available,
        windows: Availability::Available,
        server,
    }
}

fn state_with_offer(install: HerdrInstall, onboarding: bool) -> ClientShellState {
    let config = ClientShellConfig::from_config(&Config::default())
        .with_startup_onboarding(onboarding)
        .with_startup_herdr_import(Some(install));
    let mut state = ClientShellState::new(config);
    state.set_snapshot(Box::new(snapshot()));
    state.set_pane_surface(surface());
    state
}

fn screen_text(state: &mut ClientShellState) -> String {
    let frame = state.compose(106, 30).expect("import frame");
    frame
        .cells
        .chunks(frame.width as usize)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn plan(state: &ClientShellState) -> crate::herdr_import::ImportPlan {
    match &state.overlay {
        Some(ClientShellOverlay::HerdrImport(import)) => import.plan,
        other => panic!("expected the import offer, got {other:?}"),
    }
}

fn text(value: &str) -> RawInputEvent {
    RawInputEvent::Text(crate::input::TextCommit::new(value))
}

fn key(code: KeyCode) -> RawInputEvent {
    RawInputEvent::Key(crate::input::TerminalKey::new(code, KeyModifiers::empty()))
}

fn click(rect: Rect) -> RawInputEvent {
    RawInputEvent::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn import_offer_shows_before_onboarding_with_each_item_and_reason() {
    let root = scratch_root("render");
    let mut state = state_with_offer(
        install(
            &root,
            Availability::Unavailable("Herdr's server isn't running".into()),
        ),
        true,
    );
    let text = screen_text(&mut state);
    assert!(text.contains("Bring your Herdr setup into oasis"));
    assert!(text.contains("[x] Settings"));
    assert!(text.contains("[x] Saved machines"));
    assert!(text.contains("[x] Windows"));
    assert!(text.contains("[ ] Take over Herdr's server"));
    assert!(text.contains("Herdr's server isn't running"));
    assert!(text.contains("reopened as new shells"));
    assert_eq!(state.hits.overlay_rows.len(), 4);
    assert!(!state.hits.overlay_primary.is_empty());
    assert!(!state.hits.overlay_clear.is_empty());
}

#[test]
fn import_offer_is_modal_and_toggles_from_the_keyboard() {
    let root = scratch_root("keys");
    let mut state = state_with_offer(install(&root, Availability::Available), false);
    screen_text(&mut state);
    let typed = state.handle_raw_events(vec![text("a")]);
    assert!(typed.requests.is_empty(), "keys never reach the pane");
    let pasted = state.handle_raw_events(vec![RawInputEvent::Paste("secret".into())]);
    assert!(pasted.requests.is_empty());

    state.handle_raw_events(vec![text(" ")]);
    assert!(!plan(&state).settings, "space toggles the highlighted item");
    state.handle_raw_events(vec![
        key(KeyCode::Down),
        key(KeyCode::Down),
        key(KeyCode::Down),
    ]);
    let mut toggled = state.handle_raw_events(vec![key(KeyCode::Char(' '))]);
    assert!(toggled.repaint);
    let chosen = plan(&state);
    assert!(
        chosen.server,
        "the server can be chosen when it is available"
    );
    assert!(chosen.windows, "taking over the server brings the windows");
    toggled = state.handle_raw_events(vec![key(KeyCode::Char('x'))]);
    assert!(toggled.repaint);
    assert!(!plan(&state).server);
}

#[test]
fn unavailable_items_cannot_be_selected() {
    let root = scratch_root("unavailable");
    let mut offered = install(&root, Availability::Unavailable("no".into()));
    offered.remotes = Availability::Unavailable("no saved machines found".into());
    let mut state = state_with_offer(offered, false);
    assert!(!plan(&state).remotes);
    state.handle_raw_events(vec![key(KeyCode::Down), text(" ")]);
    assert!(!plan(&state).remotes);
    state.handle_raw_events(vec![key(KeyCode::Down), key(KeyCode::Down), text(" ")]);
    assert!(!plan(&state).server);
}

#[test]
fn import_queues_the_plan_and_leaves_the_client() {
    let root = scratch_root("import");
    let offered = install(&root, Availability::Available);
    let mut state = state_with_offer(offered.clone(), true);
    screen_text(&mut state);
    let outcome = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert!(outcome.detach, "the import runs after the client exits");
    let (pending, plan) = crate::herdr_import::take_pending().expect("queued import");
    assert_eq!(pending, offered);
    assert_eq!(plan, offered.default_plan());
    assert!(state.overlay.is_none());
    assert!(
        crate::herdr_import::decision(&offered.dirs.oasis_state).is_none(),
        "the decision is recorded once the import has run"
    );
}

#[test]
fn skip_remembers_the_choice_and_continues_to_onboarding() {
    let root = scratch_root("skip");
    let offered = install(&root, Availability::Available);
    let mut state = state_with_offer(offered.clone(), true);
    let outcome = state.handle_raw_events(vec![key(KeyCode::Esc)]);
    assert!(!outcome.detach);
    assert!(matches!(
        state.overlay,
        Some(ClientShellOverlay::Onboarding)
    ));
    assert_eq!(
        crate::herdr_import::decision(&offered.dirs.oasis_state),
        Some(crate::herdr_import::Decision::Skipped)
    );
    assert!(crate::herdr_import::take_pending().is_none());
    state.reset_endpoint_projection();
    assert!(
        !matches!(state.overlay, Some(ClientShellOverlay::HerdrImport(_))),
        "a skipped offer does not come back"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn import_offer_works_with_the_mouse() {
    let root = scratch_root("mouse");
    let offered = install(&root, Availability::Available);
    let mut state = state_with_offer(offered.clone(), false);
    screen_text(&mut state);
    let (windows_row, _) = state.hits.overlay_rows[2];
    state.handle_raw_events(vec![click(windows_row)]);
    assert!(!plan(&state).windows, "clicking a row toggles it");
    screen_text(&mut state);
    let skip = state.hits.overlay_clear;
    let outcome = state.handle_raw_events(vec![click(skip)]);
    assert!(!outcome.detach);
    assert!(state.overlay.is_none());
    assert_eq!(
        crate::herdr_import::decision(&offered.dirs.oasis_state),
        Some(crate::herdr_import::Decision::Skipped)
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn choosing_nothing_and_importing_counts_as_skipping() {
    let root = scratch_root("empty");
    let mut offered = install(&root, Availability::Unavailable("no".into()));
    offered.remotes = Availability::Unavailable("no".into());
    offered.windows = Availability::Unavailable("no".into());
    let mut state = state_with_offer(offered.clone(), false);
    state.handle_raw_events(vec![text(" ")]);
    assert!(plan(&state).is_empty());
    let outcome = state.handle_raw_events(vec![key(KeyCode::Enter)]);
    assert!(!outcome.detach);
    assert!(crate::herdr_import::take_pending().is_none());
    assert_eq!(
        crate::herdr_import::decision(&offered.dirs.oasis_state),
        Some(crate::herdr_import::Decision::Skipped)
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn import_item_order_matches_the_rows() {
    assert_eq!(
        crate::herdr_import::IMPORT_ITEMS,
        [
            ImportItem::Settings,
            ImportItem::Remotes,
            ImportItem::Windows,
            ImportItem::Server
        ]
    );
}
