//! The clawmux window: a native sidebar and tab bar built from the server's snapshot, the
//! server-rendered terminal surface, and oasis's own keybinds.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use oasis_core::embed::keys::{ActionContext, KeyEngine, KeyMode, KeyOutcome, KeybindAction};
use oasis_core::embed::sidebar::{
    notification_delivery, NotificationToast, SidebarModel, SidebarPrefs, ToastDelivery,
};
use oasis_core::embed::wire::{
    AgentStatus, ClientMouseButton, ClientMouseKind, ClientMousePosition, ClientPaneInputEvent,
    ClientShellSnapshot, PaneSurfacePane,
};
use serde_json::json;

use crate::conn::{Conn, Geometry, Machine};
use crate::grid::Grid;
use crate::keys::{self, Key};
use crate::sidebar::{dot, SidebarView, Target};
use crate::termview::{Selection, TermView};
use crate::theme::Theme;
use oasis_core::embed::{CoreEvent, EndpointId, EndpointStatus, SurfaceUpdate};

pub struct App {
    application: gtk::Application,
    window: gtk::ApplicationWindow,
    view: TermView,
    grid: Rc<RefCell<Grid>>,
    conn: RefCell<Option<Rc<Conn>>>,
    machines: RefCell<Vec<Machine>>,
    snapshot: RefCell<Option<Box<ClientShellSnapshot>>>,
    keys: RefCell<KeyEngine>,
    sidebar: Rc<SidebarView>,
    model: RefCell<Option<SidebarModel>>,
    prefs: RefCell<SidebarPrefs>,
    previous_pane: RefCell<Option<String>>,
    notification_pane: RefCell<Option<String>>,
    tabs: gtk::Box,
    banner: gtk::Revealer,
    banner_label: gtk::Label,
    banner_timer: RefCell<Option<glib::SourceId>>,
    theme: RefCell<Theme>,
    css: gtk::CssProvider,
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

        let sidebar = SidebarView::new();

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
        root.append(&sidebar.root);
        root.append(&main);
        window.set_child(Some(&root));

        let app = Rc::new(Self {
            application: application.clone(),
            window,
            view,
            grid,
            conn: RefCell::new(None),
            machines: RefCell::new(Vec::new()),
            snapshot: RefCell::new(None),
            keys: RefCell::new(KeyEngine::new(oasis_core::embed::keys::load())),
            sidebar,
            model: RefCell::new(None),
            prefs: RefCell::new(SidebarPrefs::load()),
            previous_pane: RefCell::new(None),
            notification_pane: RefCell::new(None),
            tabs,
            banner,
            banner_label,
            banner_timer: RefCell::new(None),
            theme: RefCell::new(theme),
            css,
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
        self.sidebar
            .connect(move |target, row, button| app.on_sidebar(target, row, button));
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

    /// Steps the terminal font by `zoom` points, or with `None` returns to the theme's size.
    fn zoom_font(&self, zoom: Option<f64>) {
        let size = zoom.map(|step| {
            (self.theme.borrow().font_size + step)
                .clamp(crate::theme::MIN_FONT_SIZE, crate::theme::MAX_FONT_SIZE)
        });
        crate::theme::save_font_size(size);
        self.reload_theme();
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
        let conn = match Conn::start(&self.geometry(), self.prefs.borrow().clone()) {
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
                        self.track_previous_pane(&snapshot);
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
        if let Some(model) = drained.sidebar {
            self.model.replace(Some(model));
            chrome_changed = true;
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
            CoreEvent::Notification {
                endpoint,
                notification,
            } => {
                let active = self.conn.borrow().as_ref().map(|conn| conn.active());
                let target_active = active.as_ref() == Some(&endpoint)
                    && notification.pane_id.is_some()
                    && notification.pane_id == self.focused_pane_id()
                    && self.window.is_visible();
                // The same rules the TUI follows, from the user's [ui.toast] delivery.
                let delivery = notification_delivery(
                    notification.kind,
                    false,
                    toast_delivery(),
                    target_active,
                    Some(self.window.is_active()),
                );
                self.notification_pane.replace(notification.pane_id.clone());
                match delivery.toast {
                    NotificationToast::None => {}
                    NotificationToast::InApp => {
                        let text = match notification.body.as_deref() {
                            Some(body) => format!("{} — {body}", notification.title),
                            None => notification.title.clone(),
                        };
                        self.show_banner(&text, Some(5));
                    }
                    // A GUI has no outer terminal to notify through; the desktop stands in.
                    NotificationToast::Terminal | NotificationToast::System => {
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
                }
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
        let active = self
            .conn
            .borrow()
            .as_ref()
            .map(|conn| conn.active().as_str().to_owned())
            .unwrap_or_default();
        if let Some(model) = self.model.borrow().as_ref() {
            self.sidebar.render(model, &active);
        }
        self.update_status();
        let snapshot = self.snapshot.borrow();
        let Some(snapshot) = snapshot.as_ref() else {
            while let Some(child) = self.tabs.first_child() {
                self.tabs.remove(&child);
            }
            return;
        };
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
                self.tabs.append(&self.tab_pill(tab));
            }
            let add = gtk::Button::with_label("+");
            add.add_css_class("tab-add");
            add.set_tooltip_text(Some("New tab"));
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

    /// One tab as a gliff-style pill: the label, and a slot that shows the tab's agent
    /// status and turns into a close button under the pointer.
    fn tab_pill(self: &Rc<Self>, tab: &oasis_core::embed::wire::ClientShellTab) -> gtk::Box {
        let mut text = if tab.custom_label {
            format!("{} {}", tab.number, tab.label)
        } else {
            tab.label.clone()
        };
        if tab.zoomed {
            text.push_str(" ⤢");
        }
        let label = gtk::Label::new(Some(&text));
        label.add_css_class("tab-label");
        let slot = gtk::Stack::new();
        slot.set_transition_type(gtk::StackTransitionType::Crossfade);
        slot.set_transition_duration(80);
        let status = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        status.set_halign(gtk::Align::Center);
        status.set_valign(gtk::Align::Center);
        if !matches!(tab.agent_status, AgentStatus::Idle | AgentStatus::Unknown) {
            status.append(&dot(tab.agent_status));
        }
        slot.add_named(&status, Some("status"));
        let close = gtk::Button::with_label("×");
        close.add_css_class("tab-action");
        close.set_tooltip_text(Some("Close tab"));
        let (app, id) = (self.clone(), tab.tab_id.clone());
        close.connect_clicked(move |_| app.confirm_close_tab(&id));
        slot.add_named(&close, Some("close"));
        slot.set_visible_child_name("status");

        let pill = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        pill.add_css_class("tab-pill");
        if tab.focused {
            pill.add_css_class("active");
        }
        pill.append(&label);
        pill.append(&slot);
        let hover = gtk::EventControllerMotion::new();
        let s = slot.clone();
        hover.connect_enter(move |_, _, _| s.set_visible_child_name("close"));
        let s = slot.clone();
        hover.connect_leave(move |_| s.set_visible_child_name("status"));
        pill.add_controller(hover);

        let click = gtk::GestureClick::new();
        click.set_button(0);
        let (app, id) = (self.clone(), tab.tab_id.clone());
        click.connect_pressed(move |gesture, n, _, _| match gesture.current_button() {
            gdk::BUTTON_MIDDLE => app.confirm_close_tab(&id),
            gdk::BUTTON_PRIMARY if n == 2 => app.rename_tab(&id),
            gdk::BUTTON_PRIMARY => {
                app.request("tab.focus", json!({ "tab_id": id }));
                app.view.grab_focus();
            }
            _ => {}
        });
        pill.add_controller(click);
        pill
    }

    fn update_status(&self) {
        let (leader, update) = match self.model.borrow().as_ref() {
            Some(model) => (model.footer.leader.clone(), model.footer.update_attention),
            None => (self.keys.borrow().config().primary_prefix_label(), false),
        };
        let leader = glib::markup_escape_text(&leader);
        let theme = self.theme.borrow();
        let mode = match self.keys.borrow().mode() {
            KeyMode::Terminal => None,
            KeyMode::Prefix => Some("PREFIX"),
            KeyMode::Resize => Some("RESIZE"),
            KeyMode::Navigate => Some("NAVIGATE"),
            KeyMode::Copy => Some("COPY"),
        };
        let mut markup = String::from("🌴 oasis");
        match mode {
            Some(mode) => markup.push_str(&format!(
                "  <span background=\"{}\" foreground=\"{}\" weight=\"bold\"> {mode} </span>  {leader}",
                theme.accent.css(),
                theme.sidebar.css()
            )),
            None => markup.push_str(&format!(" · leader: {leader}")),
        }
        if update {
            markup.push_str(&format!(
                "  <span foreground=\"{}\">● update ready</span>",
                theme.yellow.css()
            ));
        }
        self.sidebar.status.set_markup(&markup);
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
        debug(|| {
            format!(
                "key {keyval:?} {state:?} mode={:?}",
                self.keys.borrow().mode()
            )
        });
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
        if let Some(zoom) = font_zoom(&key) {
            self.zoom_font(zoom);
            return glib::Propagation::Stop;
        }
        let terminal_key = key.terminal_key();
        let outcomes = {
            let snapshot = self.snapshot.borrow();
            let Some(snapshot) = snapshot.as_deref() else {
                self.send_key_to_pane(&key);
                return glib::Propagation::Stop;
            };
            let (agent_order, workspace_order) = self.navigation_orders(snapshot);
            let previous = self.previous_pane.borrow().clone();
            let ctx = ActionContext {
                snapshot,
                agent_order: &agent_order,
                workspace_order: &workspace_order,
                previous_pane_id: previous.as_deref(),
                prompt_new_tab_name: false,
            };
            self.keys.borrow_mut().key(terminal_key, &ctx)
        };
        debug(|| format!("key {:?} {:?} -> {outcomes:?}", key.code, key.modifiers));
        for outcome in outcomes {
            match outcome {
                KeyOutcome::ToPane(_) => self.send_key_to_pane(&key),
                KeyOutcome::Request(request) => {
                    if let Some(conn) = self.conn.borrow().as_ref() {
                        conn.request_method(request.method);
                    }
                }
                KeyOutcome::Action(action) => self.client_action(action),
                KeyOutcome::Command(command) => self.show_banner(
                    &format!("custom command \"{}\" isn't in clawmux yet", command.label),
                    Some(4),
                ),
                KeyOutcome::ModeChanged(mode) => {
                    if matches!(mode, KeyMode::Navigate | KeyMode::Copy) {
                        // Not drawn yet: leave the mode rather than trap the keyboard.
                        self.keys.borrow_mut().set_mode(KeyMode::Terminal);
                        self.show_banner(&format!("{mode:?} mode isn't in clawmux yet"), Some(4));
                    }
                    self.update_status();
                }
                KeyOutcome::ModeKey(..) => {}
            }
        }
        glib::Propagation::Stop
    }

    /// Agent pane ids and workspace indexes in sidebar order on the active machine,
    /// as the key engine's index and next/previous actions count them.
    fn navigation_orders(&self, snapshot: &ClientShellSnapshot) -> (Vec<String>, Vec<usize>) {
        let active = self
            .conn
            .borrow()
            .as_ref()
            .map(|conn| conn.active().as_str().to_owned())
            .unwrap_or_default();
        let model = self.model.borrow();
        let Some(model) = model.as_ref() else {
            return (
                snapshot.agent_order.clone(),
                (0..snapshot.workspaces.len()).collect(),
            );
        };
        let agents = model
            .agents
            .iter()
            .filter(|agent| agent.endpoint == active)
            .map(|agent| agent.pane_id.clone())
            .collect();
        let workspaces = model
            .machines
            .iter()
            .find(|machine| machine.id == active)
            .map(|machine| {
                machine
                    .spaces
                    .iter()
                    .filter_map(|space| {
                        snapshot
                            .workspaces
                            .iter()
                            .position(|w| w.workspace_id == space.workspace_id)
                    })
                    .collect()
            })
            .unwrap_or_default();
        (agents, workspaces)
    }

    fn track_previous_pane(&self, next: &ClientShellSnapshot) {
        let current = self
            .snapshot
            .borrow()
            .as_ref()
            .and_then(|s| s.focused_pane_id.clone());
        if current.is_some() && current != next.focused_pane_id {
            self.previous_pane.replace(current);
        }
    }

    /// Actions the key engine leaves to the front end: dialogs, the sidebar, put away.
    fn client_action(self: &Rc<Self>, action: KeybindAction) {
        let workspace = self.focused_workspace_id();
        let tab = self.focused_tab_id();
        let pane = self.focused_pane_id();
        match action {
            KeybindAction::NewWorkspace => self.request(
                "workspace.create",
                json!({ "source_workspace_id": workspace, "focus": true }),
            ),
            KeybindAction::RenameWorkspace => {
                if let Some(id) = workspace {
                    self.rename_workspace(&id);
                }
            }
            KeybindAction::CloseWorkspace => {
                if let Some(id) = workspace {
                    self.confirm_close_workspace(&id);
                }
            }
            KeybindAction::NewTab => self.request(
                "tab.create",
                json!({ "workspace_id": workspace, "focus": true }),
            ),
            KeybindAction::RenameTab => {
                if let Some(id) = tab {
                    self.rename_tab(&id);
                }
            }
            KeybindAction::CloseTab => {
                if let Some(id) = tab {
                    self.confirm_close_tab(&id);
                }
            }
            KeybindAction::RenamePane => {
                if let Some(id) = pane {
                    let app = self.clone();
                    self.prompt("Rename pane", "", move |label| {
                        app.request("pane.rename", json!({ "pane_id": id, "label": label }));
                    });
                }
            }
            KeybindAction::ClosePane => {
                if let Some(id) = pane {
                    let app = self.clone();
                    self.confirm("Close this pane?", move || {
                        app.request("pane.close", json!({ "pane_id": id }));
                    });
                }
            }
            KeybindAction::ToggleSidebar => self
                .sidebar
                .root
                .set_visible(!self.sidebar.root.is_visible()),
            KeybindAction::Detach => self.put_away(),
            KeybindAction::EnterResizeMode => {
                self.keys.borrow_mut().set_mode(KeyMode::Resize);
                self.update_status();
            }
            KeybindAction::OpenNotificationTarget => {
                if let Some(id) = self.notification_pane.borrow().clone() {
                    self.request("pane.focus", json!({ "pane_id": id }));
                }
            }
            KeybindAction::ReloadConfig => {
                self.keys
                    .borrow_mut()
                    .set_config(oasis_core::embed::keys::load());
                let prefs = SidebarPrefs::load();
                if let Some(conn) = self.conn.borrow().as_ref() {
                    conn.set_sidebar_prefs(prefs.clone());
                }
                self.prefs.replace(prefs);
                self.reload_theme();
                self.update_status();
                self.request("server.reload_config", json!({}));
            }
            other => self.show_banner(&format!("{other:?} isn't in clawmux yet"), Some(4)),
        }
    }

    // ----- sidebar -----

    fn on_sidebar(self: &Rc<Self>, target: &Target, row: Option<&gtk::ListBoxRow>, button: u32) {
        match target {
            Target::Machine { endpoint } => self.switch_machine_str(endpoint),
            Target::Space {
                endpoint,
                workspace_id,
            } => {
                self.switch_machine_str(endpoint);
                if button == gdk::BUTTON_SECONDARY {
                    if let Some(row) = row {
                        self.space_menu(row, workspace_id, 0.0, 0.0);
                    }
                    return;
                }
                self.request("workspace.focus", json!({ "workspace_id": workspace_id }));
            }
            Target::Agent { endpoint, pane_id } => {
                self.switch_machine_str(endpoint);
                self.request("pane.focus", json!({ "pane_id": pane_id }));
            }
            Target::GroupToggle {
                endpoint,
                key,
                collapsed,
            } => {
                let prefs = {
                    let mut prefs = self.prefs.borrow_mut();
                    let groups = prefs.collapsed_groups.entry(endpoint.clone()).or_default();
                    if *collapsed {
                        groups.remove(key);
                    } else {
                        groups.insert(key.clone());
                    }
                    prefs.clone()
                };
                if let Some(conn) = self.conn.borrow().as_ref() {
                    conn.set_sidebar_prefs(prefs);
                }
            }
        }
        self.view.grab_focus();
    }

    fn switch_machine_str(self: &Rc<Self>, endpoint: &str) {
        let id = self
            .machines
            .borrow()
            .iter()
            .find(|machine| machine.id.as_str() == endpoint)
            .map(|machine| machine.id.clone());
        let active = self.conn.borrow().as_ref().map(|conn| conn.active());
        if let Some(id) = id {
            if Some(&id) != active.as_ref() {
                self.switch_machine(&id);
            }
        }
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

/// Ghostty-style font zoom: ctrl+- smaller, ctrl+= or ctrl++ bigger, ctrl+0 reset.
fn font_zoom(key: &Key) -> Option<Option<f64>> {
    let mods = key.modifiers - KeyModifiers::SHIFT;
    if mods != KeyModifiers::CONTROL {
        return None;
    }
    match key.code {
        KeyCode::Char('-') => Some(Some(-1.0)),
        KeyCode::Char('=') | KeyCode::Char('+') => Some(Some(1.0)),
        KeyCode::Char('0') => Some(None),
        _ => None,
    }
}

/// The user's `[ui.toast] delivery` from the oasis config; oasis defaults to off.
fn toast_delivery() -> ToastDelivery {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".config"))
        })
        .unwrap_or_default();
    let text = std::fs::read_to_string(dir.join("oasis/config.toml")).unwrap_or_default();
    toast_delivery_from(&text)
}

fn toast_delivery_from(text: &str) -> ToastDelivery {
    let Ok(config) = text.parse::<toml::Table>() else {
        return ToastDelivery::Off;
    };
    let toast = config
        .get("ui")
        .and_then(|ui| ui.get("toast"))
        .and_then(|toast| toast.as_table());
    match toast
        .and_then(|toast| toast.get("delivery"))
        .and_then(|delivery| delivery.as_str())
    {
        Some("herdr") => ToastDelivery::Herdr,
        Some("terminal") => ToastDelivery::Terminal,
        Some("system") => ToastDelivery::System,
        Some(_) => ToastDelivery::Off,
        // Older configs only said `enabled = true`, which meant in-app toasts.
        None if toast
            .and_then(|toast| toast.get("enabled"))
            .and_then(|enabled| enabled.as_bool())
            == Some(true) =>
        {
            ToastDelivery::Herdr
        }
        None => ToastDelivery::Off,
    }
}

fn debug(message: impl FnOnce() -> String) {
    if std::env::var_os("CLAWMUX_DEBUG").is_some() {
        eprintln!("clawmux: {}", message());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_delivery_follows_the_oasis_config_and_defaults_to_off() {
        assert_eq!(toast_delivery_from(""), ToastDelivery::Off);
        assert_eq!(
            toast_delivery_from("[keys]\nprefix = \"ctrl+space\"\n"),
            ToastDelivery::Off
        );
        assert_eq!(
            toast_delivery_from("[ui.toast]\ndelivery = \"system\"\n"),
            ToastDelivery::System
        );
        assert_eq!(
            toast_delivery_from("[ui.toast]\ndelivery = \"herdr\"\n"),
            ToastDelivery::Herdr
        );
        assert_eq!(
            toast_delivery_from("[ui.toast]\nenabled = true\n"),
            ToastDelivery::Herdr
        );
        assert_eq!(
            toast_delivery_from("[ui.toast]\ndelivery = \"off\"\nenabled = true\n"),
            ToastDelivery::Off
        );
    }
}
