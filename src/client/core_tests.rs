use super::*;
use crate::protocol::{CellData, CursorState, FrameData, SurfaceRect};

const WIDTH: u16 = 6;
const HEIGHT: u16 = 4;

fn pane(content_revision: u64) -> PaneSurfacePane {
    let rect = SurfaceRect {
        x: 0,
        y: 0,
        width: WIDTH,
        height: HEIGHT,
    };
    PaneSurfacePane {
        pane_id: "w1:p1".into(),
        content_revision,
        rect,
        inner_rect: rect,
        scrollbar_rect: None,
        scroll: None,
        focused: true,
        mouse_reporting: false,
        sgr_pixel_mouse: false,
        alternate_screen_active: false,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn surface(revision: u64) -> PaneSurfaceFrame {
    PaneSurfaceFrame {
        boot_id: "boot".into(),
        projection_revision: 1,
        surface_revision: revision,
        frame: FrameData::from_ratatui_buffer(
            &ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, WIDTH, HEIGHT)),
            None,
        ),
        panes: vec![pane(1)],
        splits: Vec::new(),
        popup: None,
        graphics: Default::default(),
    }
}

fn cells(symbol: &str, count: usize) -> Vec<CellData> {
    let mut cell = surface(0).frame.cells[0].clone();
    cell.symbol = symbol.into();
    vec![cell; count]
}

fn patch(base: u64, y: u16, x: u16, symbol: &str, cursor_x: u16) -> PaneSurfacePatch {
    PaneSurfacePatch {
        boot_id: "boot".into(),
        projection_revision: 1,
        base_surface_revision: base,
        surface_revision: base + 1,
        rows: vec![PaneSurfacePatchRow {
            x,
            y,
            cells: cells(symbol, 2),
        }],
        panes: vec![pane(base + 1)],
        cursor: Some(CursorState {
            x: cursor_x,
            y,
            visible: true,
            shape: Default::default(),
        }),
    }
}

fn local() -> EndpointId {
    EndpointId::local()
}

fn row_text(row: &PaneSurfacePatchRow) -> String {
    row.cells.iter().map(|cell| cell.symbol.as_str()).collect()
}

#[test]
fn patches_after_a_full_surface_coalesce_into_that_full_surface() {
    let mut outbox = Outbox::default();
    outbox.surface_full(&local(), "Local", surface(1));
    assert!(outbox.surface_patch(&local(), "Local", &patch(1, 0, 0, "a", 2)));
    assert!(outbox.surface_patch(&local(), "Local", &patch(2, 2, 3, "b", 5)));

    let drained = outbox.take();
    let [update] = drained.endpoints.as_slice() else {
        panic!("one endpoint update");
    };
    let Some(SurfaceUpdate::Full(full)) = &update.surface else {
        panic!("a full surface stays full");
    };
    assert_eq!(full.surface_revision, 3);
    assert_eq!(full.frame.cells[0].symbol, "a");
    assert_eq!(full.frame.cells[2 * usize::from(WIDTH) + 3].symbol, "b");
    assert_eq!(full.frame.cursor.as_ref().map(|cursor| cursor.x), Some(5));
}

#[test]
fn patches_between_drains_become_one_patch_of_merged_full_rows() {
    let mut outbox = Outbox::default();
    outbox.surface_full(&local(), "Local", surface(1));
    let _ = outbox.take();

    for (index, (y, x, symbol)) in [(1, 0, "a"), (3, 2, "b"), (1, 4, "c")].iter().enumerate() {
        let base = index as u64 + 1;
        assert!(outbox.surface_patch(&local(), "Local", &patch(base, *y, *x, symbol, base as u16)));
    }
    let drained = outbox.take();
    let Some(SurfaceUpdate::Patch {
        surface_revision,
        rows,
        panes,
        cursor,
    }) = &drained.endpoints[0].surface
    else {
        panic!("only patches arrived");
    };
    assert_eq!(*surface_revision, 4);
    assert_eq!(rows.iter().map(|row| row.y).collect::<Vec<_>>(), [1, 3]);
    assert!(rows
        .iter()
        .all(|row| row.x == 0 && row.cells.len() == usize::from(WIDTH)));
    assert_eq!(row_text(&rows[0]), "aa  cc");
    assert_eq!(row_text(&rows[1]), "  bb  ");
    assert_eq!(panes.len(), 1, "pane metadata is merged by pane id");
    assert_eq!(panes[0].content_revision, 4);
    assert_eq!(cursor.as_ref().map(|cursor| cursor.x), Some(3));
    assert!(outbox.take().is_empty());
}

#[test]
fn a_patch_for_another_base_revision_is_dropped() {
    let mut outbox = Outbox::default();
    outbox.surface_full(&local(), "Local", surface(1));
    let _ = outbox.take();
    assert!(!outbox.surface_patch(&local(), "Local", &patch(7, 0, 0, "x", 0)));
    assert!(outbox.take().is_empty());
    assert!(!outbox.surface_patch(&EndpointId("ssh:none".into()), "x", &patch(1, 0, 0, "x", 0)));
}

#[test]
fn discrete_events_stay_ordered_and_bounded() {
    let mut outbox = Outbox::default();
    for count in 0..(MAX_QUEUED_EVENTS as u16 + 3) {
        outbox.event(CoreEvent::Bell {
            endpoint: local(),
            count,
        });
    }
    let drained = outbox.take();
    assert_eq!(drained.dropped_events, 3);
    let counts = drained
        .events
        .iter()
        .map(|event| match event {
            CoreEvent::Bell { count, .. } => *count,
            other => panic!("unexpected {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(counts.len(), MAX_QUEUED_EVENTS);
    assert!(counts.windows(2).all(|pair| pair[0] + 1 == pair[1]));
    assert_eq!(counts[0], 3);
}

#[cfg(unix)]
fn wake_pending(shared: &Shared) -> bool {
    let mut descriptor = libc::pollfd {
        fd: shared.wake.raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    unsafe { libc::poll(&mut descriptor, 1, 0) > 0 }
}

#[cfg(unix)]
fn pipe_bytes(shared: &Shared) -> usize {
    let mut available: libc::c_int = 0;
    unsafe { libc::ioctl(shared.wake.raw_fd(), libc::FIONREAD, &mut available) };
    available as usize
}

#[cfg(unix)]
#[test]
fn the_wake_is_raised_once_per_empty_to_non_empty_transition() {
    let shared = Shared {
        outbox: Mutex::new(Outbox::default()),
        wake: crate::platform::CoreWake::new().unwrap(),
    };
    assert!(!wake_pending(&shared), "idle: nothing to wake for");
    for count in 0..5 {
        shared.publish(|outbox| {
            outbox.event(CoreEvent::Bell {
                endpoint: local(),
                count,
            })
        });
    }
    assert_eq!(pipe_bytes(&shared), 1, "one wake for five changes");

    let drained = {
        let mut outbox = shared.outbox.lock().unwrap();
        shared.wake.clear();
        outbox.take()
    };
    assert_eq!(drained.events.len(), 5);
    assert!(!wake_pending(&shared), "drain clears the wake");

    shared.publish(|_| {});
    assert!(
        !wake_pending(&shared),
        "a publish that changes nothing does not wake"
    );
    shared.publish(|outbox| outbox.surface_full(&local(), "Local", surface(1)));
    assert_eq!(pipe_bytes(&shared), 1);
}

#[cfg(unix)]
#[test]
fn sidebar_changes_wake_once_and_drain_while_unchanged_snapshots_stay_quiet() {
    let shared = Shared {
        outbox: Mutex::new(Outbox::default()),
        wake: crate::platform::CoreWake::new().unwrap(),
    };
    let mut sidebar = CoreSidebar::new(super::super::model::SidebarPrefs::default());
    let order = [ClientEndpointId::Local];
    let labels = HashMap::from([(ClientEndpointId::Local, "Local".to_owned())]);
    sidebar.online.insert(ClientEndpointId::Local);
    let mut snapshot = crate::client::model::tests::snapshot(
        vec![crate::client::model::tests::workspace(
            "ws",
            1,
            crate::api::schema::AgentStatus::Idle,
        )],
        Vec::new(),
    );
    sidebar
        .snapshots
        .insert(ClientEndpointId::Local, Box::new(snapshot.clone()));

    publish_sidebar(&shared, &mut sidebar, &order, &labels);
    assert_eq!(pipe_bytes(&shared), 1);
    let drained = {
        let mut outbox = shared.outbox.lock().unwrap();
        shared.wake.clear();
        outbox.take()
    };
    let model = drained.sidebar.expect("first model is delivered");
    assert_eq!(model.machines[0].label, "Local");
    assert_eq!(model.revision(), 1);

    snapshot.revision += 1;
    sidebar
        .snapshots
        .insert(ClientEndpointId::Local, Box::new(snapshot.clone()));
    publish_sidebar(&shared, &mut sidebar, &order, &labels);
    assert!(!wake_pending(&shared), "an unchanged sidebar does not wake");

    snapshot.workspaces[0].label = "renamed".into();
    sidebar
        .snapshots
        .insert(ClientEndpointId::Local, Box::new(snapshot));
    publish_sidebar(&shared, &mut sidebar, &order, &labels);
    assert!(wake_pending(&shared));
    let drained = shared.outbox.lock().unwrap().take();
    assert_eq!(drained.sidebar.map(|model| model.revision()), Some(2));
}
