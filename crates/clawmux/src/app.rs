//! The clawmux window: a native sidebar and tab bar built from the server's snapshot, the
//! server-rendered terminal surface, and oasis's own keybinds.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use oasis_core::embed::keys::{ActionKeybinds, IndexedKeybind, LiveKeybindConfig};
use oasis_core::embed::wire::{
    AgentStatus, ClientMouseButton, ClientMouseKind, ClientMousePosition, ClientPaneInputEvent,
    ClientShellSnapshot, PaneSurfacePane,
};
use serde_json::json;

use crate::conn::{Conn, Geometry, Machine};
use crate::grid::Grid;
use crate::keys::{self, Key};
use crate::termview::{Selection, TermView};
use crate::theme::Theme;
use oasis_core::embed::{CoreEvent, EndpointId, EndpointStatus, SurfaceUpdate};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    NewWorkspace,
    CloseWorkspace,
    RenameWorkspace,
    PreviousWorkspace,
    NextWorkspace,
    SwitchWorkspace(usize),
    NewTab,
    CloseTab,
    RenameTab,
    PreviousTab,
    NextTab,
    MoveTab(bool),
    SwitchTab(usize),
    FocusPane(&'static str),
    SwapPane(&'static str),
    ResizePane(&'static str),
    CyclePane(bool),
    SplitRight,
    SplitDown,
    ClosePane,
    Zoom,
    ClearPane,
    EditScrollback,
    ToggleSidebar,
    PreviousAgent,
    NextAgent,
    FocusAgent(usize),
    Detach,
    ReloadConfig,
}

pub struct App {
    application: gtk::Application,
    window: gtk::ApplicationWindow,
    view: TermView,
    grid: Rc<RefCell<Grid>>,
    conn: RefCell<Option<Rc<Conn>>>,
    machines: RefCell<Vec<Machine>>,
    machine_ids: RefCell<Vec<EndpointId>>,
    machines_list: gtk::ListBox,
    machines_heading: gtk::Label,
    snapshot: RefCell<Option<Box<ClientShellSnapshot>>>,
    keybinds: RefCell<LiveKeybindConfig>,
    prefix: Cell<bool>,
    sidebar: gtk::Box,
    spaces: gtk::ListBox,
    agents: gtk::ListBox,
    agents_heading: gtk::Label,
    tabs: gtk::Box,
    status: gtk::Label,
    banner: gtk::Revealer,
    banner_label: gtk::Label,
    banner_timer: RefCell<Option<glib::SourceId>>,
    theme: RefCell<Theme>,
    css: gtk::CssProvider,
    space_ids: RefCell<Vec<String>>,
    agent_ids: RefCell<Vec<String>>,
    pointer: Cell<(f64, f64)>,
    drag: Cell<Option<DragState>>,
    scroll_accum: Cell<f64>,
    _monitor: RefCell<Option<gio::FileMonitor>>,
}

#[derive(Clone, Copy)]
enum DragState {
    Select {
        start: (f64, f64),
        bounds: ((u16, u16), (u16, u16)),
    },
    Forward {
        button: ClientMouseButton,
    },
}

impl App {
    pub fn build(application: &gtk::Application) -> Rc<Self> {
        let theme = Theme::load();
        let css = gtk::CssProvider::new();
        css.load_from_string(&theme.css());
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &css,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }

        let window = gtk::ApplicationWindow::builder()
            .application(application)
            .title("clawmux")
            .default_width(1400)
            .default_height(860)
            .build();
        window.add_css_class("clawmux");

        let grid = Rc::new(RefCell::new(Grid::new()));
        let view = TermView::new(grid.clone(), theme.clone());

        let spaces = gtk::ListBox::new();
        spaces.set_selection_mode(gtk::SelectionMode::None);
        let agents = gtk::ListBox::new();
        agents.set_selection_mode(gtk::SelectionMode::None);
        let heading = |text: &str| {
            let label = gtk::Label::new(Some(text));
            label.set_xalign(0.0);
            label.add_css_class("heading");
            label
        };
        let agents_heading = heading("AGENTS");
        let machines_heading = heading("MACHINES");
        let machines_list = gtk::ListBox::new();
        machines_list.set_selection_mode(gtk::SelectionMode::None);
        machines_heading.set_visible(false);
        machines_list.set_visible(false);
        let lists = gtk::Box::new(gtk::Orientation::Vertical, 0);
        lists.append(&machines_heading);
        lists.append(&machines_list);
        lists.append(&heading("SPACES"));
        lists.append(&spaces);
        lists.append(&agents_heading);
        lists.append(&agents);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&lists)
            .build();
        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_use_markup(true);
        status.add_css_class("clawmux-status");
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.add_css_class("clawmux-sidebar");
        sidebar.set_size_request(250, -1);
        sidebar.append(&scroller);
        sidebar.append(&status);

        let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        tabs.add_css_class("clawmux-tabs");
        let banner_label = gtk::Label::new(None);
        banner_label.set_xalign(0.0);
        banner_label.set_wrap(true);
        banner_label.add_css_class("clawmux-banner");
        let banner = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(120)
            .child(&banner_label)
            .build();
        let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main.append(&tabs);
        main.append(&banner);
        main.append(&view);
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.append(&sidebar);
        root.append(&main);
        window.set_child(Some(&root));

        let app = Rc::new(Self {
            application: application.clone(),
            window,
            view,
            grid,
            conn: RefCell::new(None),
            machines: RefCell::new(Vec::new()),
            machine_ids: RefCell::new(Vec::new()),
            machines_list,
            machines_heading,
            snapshot: RefCell::new(None),
            keybinds: RefCell::new(oasis_core::embed::keys::load()),
            prefix: Cell::new(false),
            sidebar,
            spaces,
            agents,
            agents_heading,
            tabs,
            status,
            banner,
            banner_label,
            banner_timer: RefCell::new(None),
            theme: RefCell::new(theme),
            css,
            space_ids: RefCell::new(Vec::new()),
            agent_ids: RefCell::new(Vec::new()),
            pointer: Cell::new((0.0, 0.0)),
            drag: Cell::new(None),
            scroll_accum: Cell::new(0.0),
            _monitor: RefCell::new(None),
        });
        app.wire();
        app.update_status();
        app
    }

    pub fn toggle(self: &Rc<Self>) {
        if self.window.is_visible() && self.window.is_active() {
            self.put_away();
        } else {
            self.window.present();
            self.view.grab_focus();
        }
    }

    pub fn show(&self) {
        self.window.present();
        self.view.grab_focus();
    }

    fn put_away(&self) {
        self.window.set_visible(false);
        if let Some(conn) = self.conn.borrow().as_ref() {
            conn.focus(false);
        }
    }

    fn wire(self: &Rc<Self>) {
        let app = self.clone();
        self.view
            .connect_resize(move |cols, rows| app.on_resize(cols, rows));

        let app = self.clone();
        self.window.connect_close_request(move |_| {
            app.put_away();
            glib::Propagation::Stop
        });

        let app = self.clone();
        self.window.connect_is_active_notify(move |window| {
            let active = window.is_active();
            debug(|| format!("window active={active}"));
            app.view.set_window_focused(active);
            if let Some(conn) = app.conn.borrow().as_ref() {
                conn.focus(active);
            }
        });

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let app = self.clone();
        keys.connect_key_pressed(move |_, keyval, _, state| app.on_key(keyval, state));
        self.window.add_controller(keys);

        let app = self.clone();
        self.spaces.connect_row_activated(move |_, row| {
            if let Some(id) = app.space_ids.borrow().get(row.index() as usize) {
                app.request("workspace.focus", json!({ "workspace_id": id }));
            }
            app.view.grab_focus();
        });
        let app = self.clone();
        self.machines_list.connect_row_activated(move |_, row| {
            let id = app.machine_ids.borrow().get(row.index() as usize).cloned();
            if let Some(id) = id {
                app.switch_machine(&id);
            }
            app.view.grab_focus();
        });
        let app = self.clone();
        self.agents.connect_row_activated(move |_, row| {
            if let Some(id) = app.agent_ids.borrow().get(row.index() as usize) {
                app.request("pane.focus", json!({ "pane_id": id }));
            }
            app.view.grab_focus();
        });
        self.wire_space_menu();
        self.wire_mouse();
        self.wire_theme_monitor();

        let show_pane = gio::SimpleAction::new("show-pane", Some(glib::VariantTy::STRING));
        let app = self.clone();
        show_pane.connect_activate(move |_, param| {
            if let Some(pane_id) = param.and_then(|p| p.get::<String>()) {
                app.request("pane.focus", json!({ "pane_id": pane_id }));
            }
            app.show();
        });
        self.application.add_action(&show_pane);
    }

    fn wire_space_menu(self: &Rc<Self>) {
        let click = gtk::GestureClick::new();
        click.set_button(gdk::BUTTON_SECONDARY);
        let app = self.clone();
        click.connect_pressed(move |_, _, x, y| {
            let Some(row) = app.spaces.row_at_y(y as i32) else {
                return;
            };
            let Some(id) = app.space_ids.borrow().get(row.index() as usize).cloned() else {
                return;
            };
            app.space_menu(&row, &id, x, y);
        });
        self.spaces.add_controller(click);
    }

    fn space_menu(self: &Rc<Self>, row: &gtk::ListBoxRow, workspace_id: &str, _x: f64, _y: f64) {
        let popover = gtk::Popover::new();
        let items = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let make = |label: &str| {
            let button = gtk::Button::with_label(label);
            button.add_css_class("flat");
            items.append(&button);
            button
        };
        let rename = make("Rename");
        let new_tab = make("New tab");
        let close = make("Close");
        popover.set_child(Some(&items));
        popover.set_parent(row);
        let (app, id, pop) = (self.clone(), workspace_id.to_owned(), popover.clone());
        rename.connect_clicked(move |_| {
            pop.popdown();
            app.rename_workspace(&id);
        });
        let (app, id, pop) = (self.clone(), workspace_id.to_owned(), popover.clone());
        new_tab.connect_clicked(move |_| {
            pop.popdown();
            app.request("tab.create", json!({ "workspace_id": id, "focus": true }));
        });
        let (app, id, pop) = (self.clone(), workspace_id.to_owned(), popover.clone());
        close.connect_clicked(move |_| {
            pop.popdown();
            app.confirm_close_workspace(&id);
        });
        popover.connect_closed(|popover| popover.unparent());
        popover.popup();
    }

    fn wire_mouse(self: &Rc<Self>) {
        let motion = gtk::EventControllerMotion::new();
        let app = self.clone();
        motion.connect_motion(move |_, x, y| app.pointer.set((x, y)));
        self.view.add_controller(motion);

        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        let app = self.clone();
        drag.connect_drag_begin(move |gesture, x, y| {
            let button = gesture.current_button();
            let state = gesture.current_event_state();
            app.mouse_down(button, x, y, state);
        });
        let app = self.clone();
        drag.connect_drag_update(move |gesture, dx, dy| {
            let Some((sx, sy)) = gesture.start_point() else {
                return;
            };
            app.mouse_drag(sx + dx, sy + dy, gesture.current_event_state());
        });
        let app = self.clone();
        drag.connect_drag_end(move |gesture, dx, dy| {
            let Some((sx, sy)) = gesture.start_point() else {
                return;
            };
            app.mouse_up(sx + dx, sy + dy, gesture.current_event_state());
        });
        self.view.add_controller(drag);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let app = self.clone();
        scroll.connect_scroll(move |controller, _, dy| {
            app.on_scroll(dy, controller.current_event_state());
            glib::Propagation::Stop
        });
        self.view.add_controller(scroll);
    }

    fn wire_theme_monitor(self: &Rc<Self>) {
        // Omarchy swaps the `current` theme directory when the theme changes.
        let dir = crate::theme::theme_dir();
        let Some(parent) = dir.parent() else { return };
        let file = gio::File::for_path(parent);
        let Ok(monitor) =
            file.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        else {
            return;
        };
        let app = self.clone();
        let pending = Rc::new(Cell::new(false));
        monitor.connect_changed(move |_, _, _, _| {
            if pending.replace(true) {
                return;
            }
            let (app, pending) = (app.clone(), pending.clone());
            // One theme switch touches many files; reload once after they settle.
            glib::timeout_add_local_once(Duration::from_millis(250), move || {
                pending.set(false);
                app.reload_theme();
            });
        });
        self._monitor.replace(Some(monitor));
    }

    fn reload_theme(&self) {
        let theme = Theme::load();
        self.css.load_from_string(&theme.css());
        self.view.set_theme(theme.clone());
        self.theme.replace(theme);
    }

    // ----- connection -----

    fn geometry(&self) -> Geometry {
        let (cols, rows) = self.view.size_cells();
        let m = self.view.metrics();
        Geometry {
            cols,
            rows,
            cell_width_px: m.cell_w.round() as u32,
            cell_height_px: m.cell_h.round() as u32,
        }
    }

    fn on_resize(self: &Rc<Self>, _cols: u16, _rows: u16) {
        if let Some(conn) = self.conn.borrow().as_ref() {
            conn.resize(&self.geometry());
            return;
        }
        self.connect();
    }

    /// Starts oasis's client core; from here the GTK loop wakes only on its wake fd.
    fn connect(self: &Rc<Self>) {
        let conn = match Conn::start(&self.geometry()) {
            Ok(conn) => Rc::new(conn),
            Err(error) => {
                self.show_banner(
                    &format!("could not start the oasis client core: {error}"),
                    None,
                );
                return;
            }
        };
        conn.focus(self.window.is_active());
        let fd = conn.wake_fd();
        self.conn.replace(Some(conn));
        let app = self.clone();
        glib_unix::unix_fd_add_local(fd, glib::IOCondition::IN, move |_, _| {
            app.on_wake();
            glib::ControlFlow::Continue
        });
    }

    fn on_wake(self: &Rc<Self>) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let drained = conn.drain();
        let active = conn.active();
        let mut chrome_changed = false;
        for update in drained.endpoints {
            let is_active = update.id == active;
            {
                let mut machines = self.machines.borrow_mut();
                let index = match machines.iter().position(|m| m.id == update.id) {
                    Some(index) => index,
                    None => {
                        machines.push(Machine {
                            id: update.id.clone(),
                            label: update.label.clone(),
                            status: EndpointStatus::Connecting,
                            snapshot: None,
                        });
                        chrome_changed = true;
                        machines.len() - 1
                    }
                };
                let machine = &mut machines[index];
                if let Some(status) = update.status {
                    chrome_changed |= machine.status != status;
                    machine.status = status;
                }
                if let Some(snapshot) = update.snapshot {
                    if is_active {
                        self.snapshot.replace(Some(snapshot.clone()));
                    }
                    machine.snapshot = Some(snapshot);
                    chrome_changed = true;
                }
            }
            if is_active {
                self.show_endpoint_status();
                match update.surface {
                    Some(SurfaceUpdate::Full(surface)) => {
                        self.grid.borrow_mut().set_full(*surface);
                        self.view.set_selection(None);
                        self.view.queue_draw();
                    }
                    Some(SurfaceUpdate::Patch {
                        surface_revision,
                        rows,
                        panes,
                        cursor,
                    }) => {
                        self.grid
                            .borrow_mut()
                            .apply_rows(surface_revision, rows, panes, cursor);
                        self.view.queue_draw();
                    }
                    None => {}
                }
            }
        }
        if chrome_changed {
            self.rebuild_chrome();
        }
        for event in drained.events {
            self.on_event(event);
        }
    }

    fn show_endpoint_status(self: &Rc<Self>) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let active = conn.active();
        let machines = self.machines.borrow();
        let Some(machine) = machines.iter().find(|m| m.id == active) else {
            return;
        };
        match &machine.status {
            EndpointStatus::Online => self.hide_banner(),
            EndpointStatus::Connecting => {
                self.show_banner(&format!("connecting to {}…", machine.label), None)
            }
            EndpointStatus::Reconnecting(reason) => self.show_banner(
                &format!("{} — {reason} — reconnecting…", machine.label),
                None,
            ),
            EndpointStatus::Attention(reason) => self.show_banner(
                &format!("{} needs attention: {reason}", machine.label),
                None,
            ),
        }
    }

    fn on_event(self: &Rc<Self>, event: CoreEvent) {
        match event {
            CoreEvent::Notification { notification, .. } => {
                if self.window.is_active() {
                    return;
                }
                let n = gio::Notification::new(&notification.title);
                if let Some(body) = notification.body.as_deref() {
                    n.set_body(Some(body));
                }
                if let Some(pane_id) = notification.pane_id.as_deref() {
                    n.set_default_action_and_target_value(
                        "app.show-pane",
                        Some(&pane_id.to_variant()),
                    );
                }
                self.application.send_notification(None, &n);
            }
            CoreEvent::Error { message, .. } => self.show_banner(&message, Some(6)),
            CoreEvent::Response {
                result: Err(error), ..
            } => self.show_banner(&error.message, Some(5)),
            CoreEvent::Title { title, .. } => {
                self.window
                    .set_title(Some(title.as_deref().unwrap_or("clawmux")));
            }
            CoreEvent::Clipboard { data, .. } => {
                if let Ok(text) = String::from_utf8(data) {
                    self.view.clipboard().set_text(&text);
                }
            }
            _ => {}
        }
    }

    fn switch_machine(self: &Rc<Self>, id: &EndpointId) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        let (online, snapshot) = {
            let machines = self.machines.borrow();
            let Some(machine) = machines.iter().find(|m| &m.id == id) else {
                return;
            };
            (
                machine.status == EndpointStatus::Online,
                machine.snapshot.clone(),
            )
        };
        if !online {
            conn.retry(id);
        }
        conn.set_active(id);
        self.snapshot.replace(snapshot);
        self.grid.borrow_mut().clear();
        self.view.queue_draw();
        self.rebuild_chrome();
        self.show_endpoint_status();
    }

    fn request(self: &Rc<Self>, method: &str, params: serde_json::Value) {
        let result = match self.conn.borrow().as_ref() {
            Some(conn) => conn.request(method, params),
            None => return,
        };
        if let Err(error) = result {
            self.show_banner(&error, Some(5));
        }
    }

    // ----- chrome -----

    fn rebuild_chrome(self: &Rc<Self>) {
        self.rebuild_machines();
        let snapshot = self.snapshot.borrow();
        let Some(snapshot) = snapshot.as_ref() else {
            // A machine without a snapshot yet shows empty chrome, not the last machine's.
            self.spaces.remove_all();
            self.agents.remove_all();
            while let Some(child) = self.tabs.first_child() {
                self.tabs.remove(&child);
            }
            return;
        };

        self.spaces.remove_all();
        let mut space_ids = Vec::new();
        let mut workspaces: Vec<_> = snapshot.workspaces.iter().collect();
        workspaces.sort_by_key(|w| w.number);
        for workspace in workspaces {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.append(&dot(workspace.agent_status));
            let number = gtk::Label::new(Some(&workspace.number.to_string()));
            number.add_css_class("number");
            row.append(&number);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let label = ellipsized(&workspace.label);
            text.append(&label);
            let mut meta = workspace.branch.clone().unwrap_or_default();
            if let Some((ahead, behind)) = workspace.git_ahead_behind {
                if ahead > 0 {
                    meta.push_str(&format!(" ↑{ahead}"));
                }
                if behind > 0 {
                    meta.push_str(&format!(" ↓{behind}"));
                }
            }
            for (_, value) in &workspace.tokens {
                if !value.is_empty() {
                    meta.push_str("  ");
                    meta.push_str(value);
                }
            }
            if !meta.trim().is_empty() {
                let meta = ellipsized(meta.trim());
                meta.add_css_class("meta");
                text.append(&meta);
            }
            text.set_hexpand(true);
            row.append(&text);
            let list_row = gtk::ListBoxRow::new();
            list_row.set_child(Some(&row));
            if workspace.focused {
                list_row.add_css_class("focused");
            }
            self.spaces.append(&list_row);
            space_ids.push(workspace.workspace_id.clone());
        }
        self.space_ids.replace(space_ids);

        self.agents.remove_all();
        let mut agent_ids = Vec::new();
        let ordered = ordered_agents(snapshot);
        for agent in &ordered {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.append(&dot(agent.agent_status));
            let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let name = agent
                .name
                .clone()
                .or_else(|| agent.display_agent.clone())
                .or_else(|| agent.agent.clone())
                .unwrap_or_else(|| "agent".into());
            text.append(&ellipsized(&name));
            let workspace = snapshot
                .workspaces
                .iter()
                .find(|w| w.workspace_id == agent.workspace_id)
                .map(|w| w.label.clone())
                .unwrap_or_default();
            let mut meta = status_text(agent.agent_status).to_owned();
            for (_, value) in &agent.state_labels {
                if !value.is_empty() {
                    meta = value.clone();
                }
            }
            let detail = agent
                .title
                .clone()
                .or_else(|| agent.terminal_title_stripped.clone())
                .unwrap_or(workspace);
            if !detail.is_empty() {
                meta.push_str(" · ");
                meta.push_str(&detail);
            }
            let meta = ellipsized(&meta);
            meta.add_css_class("meta");
            text.append(&meta);
            text.set_hexpand(true);
            row.append(&text);
            let list_row = gtk::ListBoxRow::new();
            list_row.set_child(Some(&row));
            if agent.focused {
                list_row.add_css_class("focused");
            }
            self.agents.append(&list_row);
            agent_ids.push(agent.pane_id.clone());
        }
        self.agents_heading.set_visible(!agent_ids.is_empty());
        self.agent_ids.replace(agent_ids);

        while let Some(child) = self.tabs.first_child() {
            self.tabs.remove(&child);
        }
        if let Some(focused_workspace) = snapshot.focused_workspace_id.as_deref() {
            let mut tabs: Vec<_> = snapshot
                .tabs
                .iter()
                .filter(|t| t.workspace_id == focused_workspace)
                .collect();
            tabs.sort_by_key(|t| t.number);
            for tab in tabs {
                let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                if !matches!(tab.agent_status, AgentStatus::Idle | AgentStatus::Unknown) {
                    content.append(&dot(tab.agent_status));
                }
                let mut label = if tab.custom_label {
                    format!("{} {}", tab.number, tab.label)
                } else {
                    tab.label.clone()
                };
                if tab.zoomed {
                    label.push_str(" ⤢");
                }
                content.append(&gtk::Label::new(Some(&label)));
                let button = gtk::Button::new();
                button.set_child(Some(&content));
                if tab.focused {
                    button.add_css_class("focused");
                }
                let (app, id) = (self.clone(), tab.tab_id.clone());
                button.connect_clicked(move |_| {
                    app.request("tab.focus", json!({ "tab_id": id }));
                    app.view.grab_focus();
                });
                let middle = gtk::GestureClick::new();
                middle.set_button(gdk::BUTTON_MIDDLE);
                let (app, id) = (self.clone(), tab.tab_id.clone());
                middle.connect_pressed(move |_, _, _, _| app.confirm_close_tab(&id));
                button.add_controller(middle);
                let double = gtk::GestureClick::new();
                let (app, id) = (self.clone(), tab.tab_id.clone());
                double.connect_pressed(move |_, n, _, _| {
                    if n == 2 {
                        app.rename_tab(&id);
                    }
                });
                button.add_controller(double);
                self.tabs.append(&button);
            }
            let add = gtk::Button::with_label("+");
            let (app, ws) = (self.clone(), focused_workspace.to_owned());
            add.connect_clicked(move |_| {
                app.request("tab.create", json!({ "workspace_id": ws, "focus": true }));
                app.view.grab_focus();
            });
            self.tabs.append(&add);
        }

        let title = snapshot
            .workspaces
            .iter()
            .find(|w| w.focused)
            .map(|w| format!("{} — clawmux", w.label))
            .unwrap_or_else(|| "clawmux".into());
        self.window.set_title(Some(&title));
    }

    fn rebuild_machines(&self) {
        self.machines_list.remove_all();
        let active = self.conn.borrow().as_ref().map(|conn| conn.active());
        let machines = self.machines.borrow();
        let mut ids = Vec::new();
        for machine in machines.iter() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let status = match &machine.status {
                EndpointStatus::Online => AgentStatus::Done,
                EndpointStatus::Connecting | EndpointStatus::Reconnecting(_) => {
                    AgentStatus::Working
                }
                EndpointStatus::Attention(_) => AgentStatus::Blocked,
            };
            row.append(&dot(status));
            let label = if machine.id.is_local() {
                "this machine".to_owned()
            } else {
                machine.label.clone()
            };
            let label = ellipsized(&label);
            label.set_hexpand(true);
            row.append(&label);
            let list_row = gtk::ListBoxRow::new();
            list_row.set_child(Some(&row));
            if active.as_ref() == Some(&machine.id) {
                list_row.add_css_class("focused");
            }
            self.machines_list.append(&list_row);
            ids.push(machine.id.clone());
        }
        let several = ids.len() > 1;
        self.machines_heading.set_visible(several);
        self.machines_list.set_visible(several);
        self.machine_ids.replace(ids);
    }

    fn update_status(&self) {
        let prefix = self
            .keybinds
            .borrow()
            .primary_prefix_label()
            .replace('&', "&amp;")
            .replace('<', "&lt;");
        let markup = if self.prefix.get() {
            let theme = self.theme.borrow();
            format!(
                "oasis  <span background=\"{}\" foreground=\"{}\" weight=\"bold\"> PREFIX </span>  {prefix}",
                theme.accent.css(),
                theme.sidebar.css()
            )
        } else {
            format!("oasis · prefix {prefix}")
        };
        self.status.set_markup(&markup);
    }

    fn show_banner(self: &Rc<Self>, message: &str, seconds: Option<u64>) {
        self.banner_label.set_text(message);
        self.banner.set_reveal_child(true);
        if let Some(timer) = self.banner_timer.take() {
            timer.remove();
        }
        if let Some(seconds) = seconds {
            let app = self.clone();
            let id = glib::timeout_add_local_once(Duration::from_secs(seconds), move || {
                app.banner_timer.take();
                app.banner.set_reveal_child(false);
            });
            self.banner_timer.replace(Some(id));
        }
    }

    fn hide_banner(&self) {
        if let Some(timer) = self.banner_timer.take() {
            timer.remove();
        }
        self.banner.set_reveal_child(false);
    }

    // ----- keys -----

    fn on_key(self: &Rc<Self>, keyval: gdk::Key, state: gdk::ModifierType) -> glib::Propagation {
        debug(|| format!("key {keyval:?} {state:?} prefix={}", self.prefix.get()));
        if keys::is_modifier(keyval) {
            return glib::Propagation::Proceed;
        }
        // Text entries in our own popovers keep their keys.
        if let Some(focus) = gtk::prelude::GtkWindowExt::focus(&self.window) {
            if focus.is::<gtk::Text>() || focus.is::<gtk::Entry>() {
                return glib::Propagation::Proceed;
            }
        }
        let Some(key) = Key::from_gdk(keyval, state) else {
            return glib::Propagation::Proceed;
        };
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        if key.is(KeyCode::Char('C'), ctrl_shift) || key.is(KeyCode::Char('c'), ctrl_shift) {
            self.copy_selection();
            return glib::Propagation::Stop;
        }
        if key.is(KeyCode::Char('V'), ctrl_shift) || key.is(KeyCode::Char('v'), ctrl_shift) {
            self.paste();
            return glib::Propagation::Stop;
        }
        let terminal_key = key.terminal_key();
        let config = self.keybinds.borrow().clone();
        if self.prefix.replace(false) {
            self.update_status();
            if config.matches_prefix(&terminal_key) {
                // Prefix twice sends the prefix key itself through, as in tmux.
                self.send_key_to_pane(&key);
            } else if let Some(action) = match_action(&config, &terminal_key, true) {
                self.run(action);
            }
            return glib::Propagation::Stop;
        }
        if config.matches_prefix(&terminal_key) {
            self.prefix.set(true);
            self.update_status();
            return glib::Propagation::Stop;
        }
        if let Some(action) = match_action(&config, &terminal_key, false) {
            self.run(action);
            return glib::Propagation::Stop;
        }
        self.send_key_to_pane(&key);
        glib::Propagation::Stop
    }

    fn send_key_to_pane(&self, key: &Key) {
        let Some(event) = key.pane_event() else {
            return;
        };
        let Some(pane_id) = self.focused_pane_id() else {
            return;
        };
        if let Some(conn) = self.conn.borrow().as_ref() {
            conn.pane_input(&pane_id, vec![event]);
        }
        self.view.set_selection(None);
    }

    fn focused_pane_id(&self) -> Option<String> {
        if let Some(pane) = self.grid.borrow().focused_pane() {
            return Some(pane.pane_id.clone());
        }
        self.snapshot
            .borrow()
            .as_ref()
            .and_then(|s| s.focused_pane_id.clone())
    }

    fn focused_workspace_id(&self) -> Option<String> {
        self.snapshot
            .borrow()
            .as_ref()
            .and_then(|s| s.focused_workspace_id.clone())
    }

    fn focused_tab_id(&self) -> Option<String> {
        self.snapshot
            .borrow()
            .as_ref()
            .and_then(|s| s.focused_tab_id.clone())
    }

    fn run(self: &Rc<Self>, action: Action) {
        let workspace = self.focused_workspace_id();
        let tab = self.focused_tab_id();
        let pane = self.focused_pane_id();
        match action {
            Action::NewWorkspace => self.request(
                "workspace.create",
                json!({ "source_workspace_id": workspace, "focus": true }),
            ),
            Action::CloseWorkspace => {
                if let Some(id) = workspace {
                    self.confirm_close_workspace(&id);
                }
            }
            Action::RenameWorkspace => {
                if let Some(id) = workspace {
                    self.rename_workspace(&id);
                }
            }
            Action::PreviousWorkspace | Action::NextWorkspace => {
                let ids = self.space_ids.borrow().clone();
                if let Some(next) =
                    step(&ids, workspace.as_deref(), action == Action::NextWorkspace)
                {
                    self.request("workspace.focus", json!({ "workspace_id": next }));
                }
            }
            Action::SwitchWorkspace(index) => {
                if let Some(id) = self.space_ids.borrow().get(index) {
                    self.request("workspace.focus", json!({ "workspace_id": id }));
                }
            }
            Action::NewTab => self.request(
                "tab.create",
                json!({ "workspace_id": workspace, "focus": true }),
            ),
            Action::CloseTab => {
                if let Some(id) = tab {
                    self.confirm_close_tab(&id);
                }
            }
            Action::RenameTab => {
                if let Some(id) = tab {
                    self.rename_tab(&id);
                }
            }
            Action::PreviousTab | Action::NextTab => {
                let ids = self.tab_ids();
                if let Some(next) = step(&ids, tab.as_deref(), action == Action::NextTab) {
                    self.request("tab.focus", json!({ "tab_id": next }));
                }
            }
            Action::MoveTab(next) => {
                let ids = self.tab_ids();
                let Some(tab) = tab else { return };
                let Some(source) = ids.iter().position(|id| *id == tab) else {
                    return;
                };
                if ids.len() <= 1 {
                    return;
                }
                let insert_index = if next {
                    if source + 1 >= ids.len() {
                        0
                    } else {
                        source + 2
                    }
                } else if source == 0 {
                    ids.len()
                } else {
                    source - 1
                };
                self.request(
                    "tab.move",
                    json!({ "tab_id": tab, "insert_index": insert_index }),
                );
            }
            Action::SwitchTab(index) => {
                if let Some(id) = self.tab_ids().get(index) {
                    self.request("tab.focus", json!({ "tab_id": id }));
                }
            }
            Action::FocusPane(direction) => self.request(
                "pane.focus_direction",
                json!({ "pane_id": pane, "direction": direction }),
            ),
            Action::SwapPane(direction) => self.request(
                "pane.swap",
                json!({ "pane_id": pane, "direction": direction }),
            ),
            Action::ResizePane(direction) => self.request(
                "pane.resize",
                json!({ "pane_id": pane, "direction": direction }),
            ),
            Action::CyclePane(next) => {
                let ids: Vec<String> = self
                    .snapshot
                    .borrow()
                    .as_ref()
                    .map(|s| {
                        s.panes
                            .iter()
                            .filter(|p| Some(&p.tab_id) == tab.as_ref())
                            .map(|p| p.pane_id.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(id) = step(&ids, pane.as_deref(), next) {
                    self.request("pane.focus", json!({ "pane_id": id }));
                }
            }
            Action::SplitRight | Action::SplitDown => self.request(
                "pane.split",
                json!({
                    "workspace_id": workspace,
                    "target_pane_id": pane,
                    "direction": if action == Action::SplitRight { "right" } else { "down" },
                    "focus": true,
                }),
            ),
            Action::ClosePane => {
                if let Some(id) = pane {
                    self.request("pane.close", json!({ "pane_id": id }));
                }
            }
            Action::Zoom => self.request("pane.zoom", json!({ "pane_id": pane, "mode": "toggle" })),
            Action::ClearPane => {
                if let Some(id) = pane {
                    self.request("pane.clear", json!({ "pane_id": id }));
                }
            }
            Action::EditScrollback => {
                if let Some(id) = pane {
                    self.request("pane.edit_scrollback", json!({ "pane_id": id }));
                }
            }
            Action::ToggleSidebar => self.sidebar.set_visible(!self.sidebar.is_visible()),
            Action::PreviousAgent | Action::NextAgent => {
                let ids = self.agent_ids.borrow().clone();
                if let Some(next) = step(&ids, pane.as_deref(), action == Action::NextAgent) {
                    self.request("pane.focus", json!({ "pane_id": next }));
                }
            }
            Action::FocusAgent(index) => {
                if let Some(id) = self.agent_ids.borrow().get(index) {
                    self.request("pane.focus", json!({ "pane_id": id }));
                }
            }
            Action::Detach => self.put_away(),
            Action::ReloadConfig => {
                self.keybinds.replace(oasis_core::embed::keys::load());
                self.update_status();
                self.reload_theme();
                self.request("server.reload_config", json!({}));
            }
        }
    }

    fn tab_ids(&self) -> Vec<String> {
        let snapshot = self.snapshot.borrow();
        let Some(snapshot) = snapshot.as_ref() else {
            return Vec::new();
        };
        let Some(workspace) = snapshot.focused_workspace_id.as_deref() else {
            return Vec::new();
        };
        let mut tabs: Vec<_> = snapshot
            .tabs
            .iter()
            .filter(|t| t.workspace_id == workspace)
            .collect();
        tabs.sort_by_key(|t| t.number);
        tabs.into_iter().map(|t| t.tab_id.clone()).collect()
    }

    // ----- dialogs -----

    fn rename_workspace(self: &Rc<Self>, workspace_id: &str) {
        let current = self
            .snapshot
            .borrow()
            .as_ref()
            .and_then(|s| s.workspaces.iter().find(|w| w.workspace_id == workspace_id))
            .map(|w| w.label.clone())
            .unwrap_or_default();
        let (app, id) = (self.clone(), workspace_id.to_owned());
        self.prompt("Rename space", &current, move |label| {
            app.request(
                "workspace.rename",
                json!({ "workspace_id": id, "label": label }),
            );
        });
    }

    fn rename_tab(self: &Rc<Self>, tab_id: &str) {
        let current = self
            .snapshot
            .borrow()
            .as_ref()
            .and_then(|s| s.tabs.iter().find(|t| t.tab_id == tab_id))
            .map(|t| t.label.clone())
            .unwrap_or_default();
        let (app, id) = (self.clone(), tab_id.to_owned());
        self.prompt("Rename tab", &current, move |label| {
            app.request("tab.rename", json!({ "tab_id": id, "label": label }));
        });
    }

    fn prompt(self: &Rc<Self>, title: &str, initial: &str, done: impl Fn(String) + 'static) {
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title(title)
            .default_width(360)
            .resizable(false)
            .build();
        let entry = gtk::Entry::new();
        entry.set_text(initial);
        entry.set_margin_top(16);
        entry.set_margin_bottom(16);
        entry.set_margin_start(16);
        entry.set_margin_end(16);
        dialog.set_child(Some(&entry));
        let (d, app) = (dialog.clone(), self.clone());
        entry.connect_activate(move |entry| {
            let text = entry.text().trim().to_owned();
            if !text.is_empty() {
                done(text);
            }
            d.close();
            app.view.grab_focus();
        });
        let keys = gtk::EventControllerKey::new();
        let d = dialog.clone();
        keys.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::Escape {
                d.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        dialog.add_controller(keys);
        dialog.present();
        entry.grab_focus();
    }

    fn confirm(self: &Rc<Self>, message: &str, action: impl FnOnce() + 'static) {
        let dialog = gtk::AlertDialog::builder()
            .message(message)
            .buttons(["Cancel", "Close"])
            .cancel_button(0)
            .default_button(1)
            .modal(true)
            .build();
        let app = self.clone();
        dialog.choose(Some(&self.window), gio::Cancellable::NONE, move |result| {
            if result == Ok(1) {
                action();
            }
            app.view.grab_focus();
        });
    }

    fn confirm_close_workspace(self: &Rc<Self>, workspace_id: &str) {
        let label = self
            .snapshot
            .borrow()
            .as_ref()
            .and_then(|s| s.workspaces.iter().find(|w| w.workspace_id == workspace_id))
            .map(|w| w.label.clone())
            .unwrap_or_else(|| "this space".into());
        let (app, id) = (self.clone(), workspace_id.to_owned());
        self.confirm(
            &format!("Close {label} and everything running in it?"),
            move || {
                app.request("workspace.close", json!({ "workspace_id": id }));
            },
        );
    }

    fn confirm_close_tab(self: &Rc<Self>, tab_id: &str) {
        let (app, id) = (self.clone(), tab_id.to_owned());
        self.confirm("Close this tab and its panes?", move || {
            app.request("tab.close", json!({ "tab_id": id }));
        });
    }

    // ----- mouse, selection, clipboard -----

    fn pane_event_position(&self, pane: &PaneSurfacePane, cell: (u16, u16)) -> ClientMousePosition {
        let inner = pane.inner_rect;
        ClientMousePosition::Cell {
            column: cell
                .0
                .saturating_sub(inner.x)
                .min(inner.width.saturating_sub(1)),
            row: cell
                .1
                .saturating_sub(inner.y)
                .min(inner.height.saturating_sub(1)),
        }
    }

    fn mouse_modifiers(state: gdk::ModifierType) -> u8 {
        let mut bits = KeyModifiers::empty();
        if state.contains(gdk::ModifierType::SHIFT_MASK) {
            bits |= KeyModifiers::SHIFT;
        }
        if state.contains(gdk::ModifierType::CONTROL_MASK) {
            bits |= KeyModifiers::CONTROL;
        }
        if state.contains(gdk::ModifierType::ALT_MASK) {
            bits |= KeyModifiers::ALT;
        }
        bits.bits()
    }

    fn forward_mouse(
        &self,
        kind: ClientMouseKind,
        cell: (u16, u16),
        state: gdk::ModifierType,
    ) -> bool {
        let grid = self.grid.borrow();
        let Some(pane) = grid.pane_at(cell.0, cell.1) else {
            return false;
        };
        let event = ClientPaneInputEvent::Mouse {
            kind,
            position: self.pane_event_position(pane, cell),
            geometry: None,
            modifiers: Self::mouse_modifiers(state),
            lines: 3,
        };
        let pane_id = pane.pane_id.clone();
        drop(grid);
        if let Some(conn) = self.conn.borrow().as_ref() {
            conn.pane_input(&pane_id, vec![event]);
        }
        true
    }

    fn mouse_down(self: &Rc<Self>, button: u32, x: f64, y: f64, state: gdk::ModifierType) {
        debug(|| {
            format!(
                "mouse down {button} at {x:.0},{y:.0} cell {:?}",
                self.view.cell_at(x, y)
            )
        });
        self.view.grab_focus();
        let cell = self.view.cell_at(x, y);
        let (pane_id, focused, reporting, inner) = {
            let grid = self.grid.borrow();
            match grid.pane_at(cell.0, cell.1) {
                Some(pane) => (
                    pane.pane_id.clone(),
                    pane.focused,
                    pane.mouse_reporting,
                    pane.inner_rect,
                ),
                None => return,
            }
        };
        let bounds = (
            (inner.x, inner.x + inner.width.saturating_sub(1)),
            (inner.y, inner.y + inner.height.saturating_sub(1)),
        );
        if !focused {
            self.request("pane.focus", json!({ "pane_id": pane_id }));
        }
        let button = match button {
            gdk::BUTTON_SECONDARY => ClientMouseButton::Right,
            gdk::BUTTON_MIDDLE => ClientMouseButton::Middle,
            _ => ClientMouseButton::Left,
        };
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        if reporting && !shift {
            self.forward_mouse(ClientMouseKind::Down(button), cell, state);
            self.drag.set(Some(DragState::Forward { button }));
            return;
        }
        if button == ClientMouseButton::Left {
            if state.contains(gdk::ModifierType::CONTROL_MASK) && self.open_link_at(cell) {
                self.drag.set(None);
                return;
            }
            self.view.set_selection(None);
            self.drag.set(Some(DragState::Select {
                start: (x, y),
                bounds,
            }));
        } else if button == ClientMouseButton::Middle {
            self.paste_primary();
            self.drag.set(None);
        }
    }

    fn mouse_drag(&self, x: f64, y: f64, state: gdk::ModifierType) {
        match self.drag.get() {
            Some(DragState::Forward { button }) => {
                self.forward_mouse(
                    ClientMouseKind::Drag(button),
                    self.view.cell_at(x, y),
                    state,
                );
            }
            Some(DragState::Select { start, bounds }) => {
                let clamp = |(c, r): (u16, u16)| {
                    (
                        c.clamp(bounds.0 .0, bounds.0 .1),
                        r.clamp(bounds.1 .0, bounds.1 .1),
                    )
                };
                let anchor = clamp(self.view.cell_at(start.0, start.1));
                let head = clamp(self.view.cell_at(x, y));
                self.view.set_selection(Some(Selection {
                    anchor,
                    head,
                    cols: bounds.0,
                }));
            }
            None => {}
        }
    }

    fn mouse_up(&self, x: f64, y: f64, state: gdk::ModifierType) {
        match self.drag.take() {
            Some(DragState::Forward { button }) => {
                self.forward_mouse(ClientMouseKind::Up(button), self.view.cell_at(x, y), state);
            }
            Some(DragState::Select { .. }) => {
                if let Some(selection) = self.view.selection() {
                    if selection.anchor != selection.head {
                        let text = self.grid.borrow().text(
                            selection.anchor,
                            selection.head,
                            selection.cols,
                        );
                        self.view.primary_clipboard().set_text(&text);
                        self.view.clipboard().set_text(&text);
                    } else {
                        self.view.set_selection(None);
                    }
                }
            }
            None => {}
        }
    }

    fn on_scroll(&self, dy: f64, state: gdk::ModifierType) {
        debug(|| format!("scroll {dy}"));
        let total = self.scroll_accum.get() + dy;
        let steps = total.trunc();
        self.scroll_accum.set(total - steps);
        if steps == 0.0 {
            return;
        }
        let (x, y) = self.pointer.get();
        let cell = self.view.cell_at(x, y);
        let kind = if steps < 0.0 {
            ClientMouseKind::ScrollUp
        } else {
            ClientMouseKind::ScrollDown
        };
        for _ in 0..(steps.abs() as usize).min(10) {
            self.forward_mouse(kind, cell, state);
        }
    }

    fn open_link_at(&self, cell: (u16, u16)) -> bool {
        let uri = {
            let grid = self.grid.borrow();
            grid.cell(cell.0, cell.1)
                .and_then(|c| c.hyperlink)
                .and_then(|index| grid.hyperlinks.get(index as usize).cloned())
        };
        let Some(uri) = uri else { return false };
        gtk::UriLauncher::new(&uri).launch(Some(&self.window), gio::Cancellable::NONE, |_| {});
        true
    }

    fn copy_selection(&self) {
        if let Some(selection) = self.view.selection() {
            let text = self
                .grid
                .borrow()
                .text(selection.anchor, selection.head, selection.cols);
            self.view.clipboard().set_text(&text);
        }
    }

    fn paste(self: &Rc<Self>) {
        let clipboard = self.view.clipboard();
        self.paste_from(clipboard);
    }

    fn paste_primary(&self) {
        let clipboard = self.view.primary_clipboard();
        let Some(pane_id) = self.focused_pane_id() else {
            return;
        };
        let conn = self.conn.borrow().clone();
        glib::spawn_future_local(async move {
            if let (Ok(Some(text)), Some(conn)) = (clipboard.read_text_future().await, conn) {
                conn.pane_input(
                    &pane_id,
                    vec![ClientPaneInputEvent::Paste(text.to_string())],
                );
            }
        });
    }

    fn paste_from(&self, clipboard: gdk::Clipboard) {
        let Some(pane_id) = self.focused_pane_id() else {
            return;
        };
        let conn = self.conn.borrow().clone();
        glib::spawn_future_local(async move {
            if let (Ok(Some(text)), Some(conn)) = (clipboard.read_text_future().await, conn) {
                conn.pane_input(
                    &pane_id,
                    vec![ClientPaneInputEvent::Paste(text.to_string())],
                );
            }
        });
    }
}

fn match_action(
    config: &LiveKeybindConfig,
    key: &oasis_core::embed::keys::TerminalKey,
    prefix: bool,
) -> Option<Action> {
    let k = &config.keybinds;
    let single: [(&ActionKeybinds, Action); 37] = [
        (&k.new_workspace, Action::NewWorkspace),
        (&k.close_workspace, Action::CloseWorkspace),
        (&k.rename_workspace, Action::RenameWorkspace),
        (&k.previous_workspace, Action::PreviousWorkspace),
        (&k.next_workspace, Action::NextWorkspace),
        (&k.new_tab, Action::NewTab),
        (&k.close_tab, Action::CloseTab),
        (&k.rename_tab, Action::RenameTab),
        (&k.previous_tab, Action::PreviousTab),
        (&k.next_tab, Action::NextTab),
        (&k.move_tab_previous, Action::MoveTab(false)),
        (&k.move_tab_next, Action::MoveTab(true)),
        (&k.focus_pane_left, Action::FocusPane("left")),
        (&k.focus_pane_down, Action::FocusPane("down")),
        (&k.focus_pane_up, Action::FocusPane("up")),
        (&k.focus_pane_right, Action::FocusPane("right")),
        (&k.swap_pane_left, Action::SwapPane("left")),
        (&k.swap_pane_down, Action::SwapPane("down")),
        (&k.swap_pane_up, Action::SwapPane("up")),
        (&k.swap_pane_right, Action::SwapPane("right")),
        (&k.resize_pane_left, Action::ResizePane("left")),
        (&k.resize_pane_down, Action::ResizePane("down")),
        (&k.resize_pane_up, Action::ResizePane("up")),
        (&k.resize_pane_right, Action::ResizePane("right")),
        (&k.cycle_pane_next, Action::CyclePane(true)),
        (&k.cycle_pane_previous, Action::CyclePane(false)),
        (&k.split_vertical, Action::SplitRight),
        (&k.split_horizontal, Action::SplitDown),
        (&k.close_pane, Action::ClosePane),
        (&k.zoom, Action::Zoom),
        (&k.clear_pane, Action::ClearPane),
        (&k.edit_scrollback, Action::EditScrollback),
        (&k.toggle_sidebar, Action::ToggleSidebar),
        (&k.previous_agent, Action::PreviousAgent),
        (&k.next_agent, Action::NextAgent),
        (&k.detach, Action::Detach),
        (&k.reload_config, Action::ReloadConfig),
    ];
    for (bindings, action) in single {
        let hit = if prefix {
            bindings.matches_prefix_key(key)
        } else {
            bindings.matches_direct_key(key)
        };
        if hit {
            return Some(action);
        }
    }
    let indexed: [(&Vec<IndexedKeybind>, fn(usize) -> Action); 3] = [
        (&k.switch_workspace, Action::SwitchWorkspace),
        (&k.switch_tab, Action::SwitchTab),
        (&k.focus_agent, Action::FocusAgent),
    ];
    for (bindings, make) in indexed {
        for binding in bindings {
            if binding.trigger.is_prefix() != prefix {
                continue;
            }
            if let Some(index) = binding.matched_index(key) {
                return Some(make(index));
            }
        }
    }
    None
}

fn debug(message: impl FnOnce() -> String) {
    if std::env::var_os("CLAWMUX_DEBUG").is_some() {
        eprintln!("clawmux: {}", message());
    }
}

fn step(ids: &[String], current: Option<&str>, forward: bool) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let index = current
        .and_then(|current| ids.iter().position(|id| id == current))
        .unwrap_or(0);
    let next = if forward {
        (index + 1) % ids.len()
    } else {
        (index + ids.len() - 1) % ids.len()
    };
    ids.get(next).cloned()
}

fn ordered_agents(
    snapshot: &ClientShellSnapshot,
) -> Vec<&oasis_core::embed::wire::ClientShellAgent> {
    let mut agents: Vec<_> = snapshot.agents.iter().collect();
    if !snapshot.agent_order.is_empty() {
        agents.sort_by_key(|agent| {
            snapshot
                .agent_order
                .iter()
                .position(|id| *id == agent.pane_id)
                .unwrap_or(usize::MAX)
        });
    }
    agents
}

fn status_text(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "needs you",
        AgentStatus::Done => "done",
        AgentStatus::Idle => "idle",
        AgentStatus::Unknown => "—",
    }
}

fn dot(status: AgentStatus) -> gtk::Widget {
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("dot");
    dot.add_css_class(match status {
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        _ => "idle",
    });
    dot.set_valign(gtk::Align::Center);
    dot.set_halign(gtk::Align::Center);
    dot.upcast()
}

fn ellipsized(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_ellipsize(pango::EllipsizeMode::End);
    label
}
