//! The clawmux window: a native sidebar and tab bar built from the server's snapshot, the
//! server-rendered terminal surface, and oasis's own keybinds.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use oasis_core::embed::keys::{
    route_key, ActionContext, KeyEngine, KeyMode, KeyOutcome, KeyStep, KeybindAction, KeybindMatch,
};
use oasis_core::embed::sidebar::{
    notification_delivery, NotificationToast, SidebarModel, SidebarPrefs,
};
use oasis_core::embed::wire::{
    AgentStatus, ClientMouseButton, ClientMouseKind, ClientMousePosition, ClientPaneInputEvent,
    ClientShellSnapshot, PaneSurfacePane,
};
use serde_json::json;

use crate::conn::{Conn, Geometry, Machine};
use crate::drawer::Drawer;
use crate::grid::Grid;
use crate::keys::{self, Key};
use crate::sidebar::{dot, SidebarView, Target};
use crate::termview::{Selection, TermView};
use crate::theme::Theme;
use oasis_core::embed::links;
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
    drawer: Rc<Drawer>,
    /// The on-screen keyboard's input context, on a touchscreen.
    keyboard: RefCell<Option<gtk::IMMulticontext>>,
    /// The font size when a pinch began.
    pinch_base: Cell<f64>,
    model: RefCell<Option<SidebarModel>>,
    prefs: RefCell<SidebarPrefs>,
    previous_pane: RefCell<Option<String>>,
    browser: Rc<crate::browser::Panel>,
    notification_pane: RefCell<Option<String>>,
    tabs: gtk::Box,
    /// Touch buttons for the focused pane's agent (interrupt, background, send now).
    quick: gtk::Box,
    quick_actions: RefCell<Vec<crate::quick::QuickAction>>,
    /// Closes the focused pane, in the touch layout's tab bar.
    pane_close: gtk::Button,
    banner: gtk::Revealer,
    banner_label: gtk::Label,
    /// The bar that says the active machine has stopped answering; kept apart from
    /// the banner so a passing notice doesn't clear it.
    stall: gtk::Revealer,
    stall_label: gtk::Label,
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
    /// A finger on the terminal: it scrolls once it moves, and taps otherwise.
    Finger {
        last_y: f64,
        moved: bool,
    },
    /// A finger held on the terminal selected the word under it; moving it
    /// extends the selection from that word.
    TouchSelect {
        word: ((u16, u16), (u16, u16)),
        bounds: ((u16, u16), (u16, u16)),
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
        let banner_label = gtk::Label::new(None);
        banner_label.set_xalign(0.0);
        banner_label.set_wrap(true);
        banner_label.add_css_class("clawmux-banner");
        let banner = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(120)
            .child(&banner_label)
            .build();
        let stall_label = gtk::Label::new(None);
        stall_label.set_xalign(0.0);
        stall_label.set_wrap(true);
        stall_label.add_css_class("clawmux-stall");
        let stall = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideDown)
            .transition_duration(120)
            .child(&stall_label)
            .build();
        let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        header.add_css_class("clawmux-tabs");
        tabs.set_hexpand(true);
        let drawer = Drawer::new(&sidebar.root, &main);
        let pane_close = gtk::Button::with_label("✕ Pane");
        pane_close.add_css_class("pane-close");
        pane_close.set_focusable(false);
        pane_close.set_tooltip_text(Some("Close this pane"));
        pane_close.set_valign(gtk::Align::Center);
        pane_close.set_visible(false);
        header.append(&drawer.button);
        header.append(&tabs);
        header.append(&pane_close);
        let quick = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        quick.add_css_class("clawmux-quick");
        quick.set_visible(false);
        main.append(&header);
        main.append(&quick);
        main.append(&stall);
        main.append(&banner);
        main.append(&view);
        window.set_child(Some(&drawer.root));

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
            drawer,
            keyboard: RefCell::new(None),
            pinch_base: Cell::new(0.0),
            model: RefCell::new(None),
            prefs: RefCell::new(SidebarPrefs::load()),
            previous_pane: RefCell::new(None),
            browser: Rc::new(crate::browser::Panel::default()),
            notification_pane: RefCell::new(None),
            tabs,
            quick,
            quick_actions: RefCell::new(Vec::new()),
            pane_close,
            banner,
            banner_label,
            stall,
            stall_label,
            banner_timer: RefCell::new(None),
            theme: RefCell::new(theme),
            css,
            pointer: Cell::new((0.0, 0.0)),
            drag: Cell::new(None),
            scroll_accum: Cell::new(0.0),
            _monitor: RefCell::new(None),
        });
        app.wire();
        app.wire_touch();
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
        // Closing the window (super+w, the title bar) quits like any app; the sessions live
        // on in the oasis server. Detach and relaunching still just hide it.
        self.window.connect_close_request(move |_| {
            app.application.quit();
            glib::Propagation::Proceed
        });

        for property in [
            "default-width",
            "default-height",
            "maximized",
            "fullscreened",
        ] {
            let app = self.clone();
            self.window
                .connect_notify_local(Some(property), move |_, _| app.fit_sidebar());
        }

        let app = self.clone();
        self.window.connect_is_active_notify(move |window| {
            let active = window.is_active();
            debug(|| format!("window active={active}"));
            app.view.set_window_focused(active);
            if active {
                // Keys always go to the terminal, whatever was clicked last.
                app.view.grab_focus();
                app.retry_waiting_machines();
            }
            if let Some(keyboard) = app.keyboard.borrow().as_ref() {
                if active {
                    keyboard.focus_in();
                } else {
                    keyboard.focus_out();
                }
            }
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

        let app = self.clone();
        self.pane_close.connect_clicked(move |_| {
            if let Some(id) = app.focused_pane_id() {
                app.confirm_close_pane(&id);
            }
        });

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

    /// A menu of actions beside `anchor`: right-click on a desktop, a long press on a
    /// touchscreen, where its buttons are sized for fingers. It hangs off the window, not
    /// the anchor, so a sidebar or tab bar rebuilt underneath doesn't take it away.
    /// Where `anchor` is, for a menu; taken before anything rebuilds the anchor away.
    fn anchor_rect(&self, anchor: &impl IsA<gtk::Widget>) -> gdk::Rectangle {
        anchor
            .compute_bounds(&self.drawer.root)
            .map(|b| {
                gdk::Rectangle::new(
                    b.x() as i32,
                    b.y() as i32,
                    (b.width() as i32).max(1),
                    (b.height() as i32).max(1),
                )
            })
            .unwrap_or_else(|| gdk::Rectangle::new(0, 0, 1, 1))
    }

    fn menu(self: &Rc<Self>, rect: gdk::Rectangle, items: Vec<MenuItem>) {
        let parent = &self.drawer.root;
        let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let popover = gtk::Popover::builder()
            .child(&column)
            .pointing_to(&rect)
            .build();
        if self.drawer.touch_layout() {
            popover.add_css_class("touch-menu");
        }
        for (label, action) in items {
            let button = gtk::Button::with_label(label);
            button.add_css_class("flat");
            if let Some(child) = button.child() {
                child.set_halign(gtk::Align::Start);
            }
            let menu = popover.downgrade();
            button.connect_clicked(move |_| {
                if let Some(menu) = menu.upgrade() {
                    menu.popdown();
                }
                action();
            });
            column.append(&button);
        }
        popover.connect_closed(|popover| popover.unparent());
        popover.set_parent(parent);
        popover.popup();
    }

    fn space_menu(self: &Rc<Self>, rect: gdk::Rectangle, workspace_id: &str) {
        let (a, b, c) = (self.clone(), self.clone(), self.clone());
        let (id1, id2, id3) = (
            workspace_id.to_owned(),
            workspace_id.to_owned(),
            workspace_id.to_owned(),
        );
        self.menu(
            rect,
            vec![
                ("Rename", Box::new(move || a.rename_workspace(&id1))),
                (
                    "New tab",
                    Box::new(move || {
                        b.request("tab.create", json!({ "workspace_id": id2, "focus": true }));
                        b.drawer.close();
                    }),
                ),
                ("Close", Box::new(move || c.confirm_close_workspace(&id3))),
            ],
        );
    }

    fn tab_menu(self: &Rc<Self>, rect: gdk::Rectangle, tab_id: &str, workspace_id: &str) {
        let (a, b, c) = (self.clone(), self.clone(), self.clone());
        let (id1, ws, id3) = (
            tab_id.to_owned(),
            workspace_id.to_owned(),
            tab_id.to_owned(),
        );
        self.menu(
            rect,
            vec![
                ("Rename tab", Box::new(move || a.rename_tab(&id1))),
                (
                    "New tab",
                    Box::new(move || {
                        b.request("tab.create", json!({ "workspace_id": ws, "focus": true }));
                    }),
                ),
                ("Close tab", Box::new(move || c.confirm_close_tab(&id3))),
            ],
        );
    }

    fn agent_menu(self: &Rc<Self>, rect: gdk::Rectangle, pane_id: &str) {
        let (a, b, c) = (self.clone(), self.clone(), self.clone());
        let (id1, id2, id3) = (pane_id.to_owned(), pane_id.to_owned(), pane_id.to_owned());
        self.menu(
            rect,
            vec![
                (
                    "Go to agent",
                    Box::new(move || {
                        a.request("pane.focus", json!({ "pane_id": id1 }));
                        a.drawer.close();
                    }),
                ),
                ("Rename pane", Box::new(move || b.rename_pane(&id2))),
                ("Close pane", Box::new(move || c.confirm_close_pane(&id3))),
            ],
        );
    }

    fn machine_menu(self: &Rc<Self>, rect: gdk::Rectangle, endpoint: &str) {
        let (a, b) = (self.clone(), self.clone());
        let (id1, id2) = (endpoint.to_owned(), endpoint.to_owned());
        self.menu(
            rect,
            vec![
                ("Switch to", Box::new(move || a.switch_machine_str(&id1))),
                (
                    "Reconnect now",
                    Box::new(move || {
                        let id = b
                            .machines
                            .borrow()
                            .iter()
                            .find(|machine| machine.id.as_str() == id2)
                            .map(|machine| machine.id.clone());
                        if let (Some(id), Some(conn)) = (id, b.conn.borrow().as_ref()) {
                            conn.retry(&id);
                        }
                    }),
                ),
            ],
        );
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
            let finger = crate::touch::from_finger(gesture);
            debug(|| {
                let event = gesture.current_event();
                format!(
                    "press finger={finger} event={:?} source={:?}",
                    event.as_ref().map(|e| e.event_type()),
                    event.and_then(|e| e.device()).map(|d| d.source())
                )
            });
            if finger {
                app.pointer.set((x, y));
                app.drag.set(Some(DragState::Finger {
                    last_y: y,
                    moved: false,
                }));
                return;
            }
            let button = gesture.current_button();
            let state = gesture.current_event_state();
            app.mouse_down(button, x, y, state);
        });
        let app = self.clone();
        drag.connect_drag_update(move |gesture, dx, dy| {
            let Some((sx, sy)) = gesture.start_point() else {
                return;
            };
            if let Some(DragState::Finger { last_y, moved }) = app.drag.get() {
                if !moved && dx.hypot(dy) < crate::touch::TAP_SLOP {
                    return;
                }
                let y = sy + dy;
                let lines = crate::touch::scrolled_lines(last_y, y, app.view.metrics().cell_h);
                app.drag.set(Some(DragState::Finger {
                    last_y: y,
                    moved: true,
                }));
                app.on_scroll(lines, gesture.current_event_state());
                return;
            }
            app.mouse_drag(sx + dx, sy + dy, gesture.current_event_state());
        });
        let app = self.clone();
        drag.connect_drag_end(move |gesture, dx, dy| {
            let Some((sx, sy)) = gesture.start_point() else {
                return;
            };
            if let Some(DragState::TouchSelect { .. }) = app.drag.get() {
                app.drag.set(None);
                app.touch_menu(sx + dx, sy + dy);
                return;
            }
            if let Some(DragState::Finger { moved, .. }) = app.drag.take() {
                if !moved {
                    app.view.set_selection(None);
                    // A tap on a link opens it, as ctrl+click does, and leaves the
                    // keyboard down.
                    if app.tap_link(sx, sy) {
                        return;
                    }
                    // A tap clicks, so apps that read the mouse and pane focus still work.
                    let state = gesture.current_event_state();
                    app.mouse_down(gdk::BUTTON_PRIMARY, sx, sy, state);
                    app.mouse_up(sx, sy, state);
                    app.show_keyboard(gesture.current_event());
                }
                return;
            }
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

    /// On a touchscreen: bigger touch targets, the on-screen keyboard, and pinch to
    /// resize the font.
    fn wire_touch(self: &Rc<Self>) {
        let touch = crate::touch::has_touchscreen();
        debug(|| format!("touchscreen={touch}"));
        if !touch {
            return;
        }
        self.drawer.set_touch(true);
        self.sync_sidebar_order();
        let app = self.clone();
        let keyboard = crate::touch::keyboard_for(&self.view, move |text| {
            if let (Some(pane_id), Some(conn)) = (app.focused_pane_id(), app.conn.borrow().as_ref())
            {
                conn.pane_input(
                    &pane_id,
                    vec![ClientPaneInputEvent::TextCommit(text.to_owned())],
                );
            }
        });
        self.keyboard.replace(Some(keyboard));

        let pinch = gtk::GestureZoom::new();
        let app = self.clone();
        pinch.connect_begin(move |_, _| {
            // The first finger began a scroll; a pinch is not one.
            app.drag.set(None);
            app.pinch_base.set(app.theme.borrow().font_size);
        });
        let app = self.clone();
        pinch.connect_scale_changed(move |_, scale| {
            let size = crate::touch::pinched_size(app.pinch_base.get(), scale);
            if size != app.theme.borrow().font_size {
                crate::theme::save_font_size(Some(size));
                app.reload_theme();
            }
        });
        self.view.add_controller(pinch);

        // Hold a finger on the terminal to select the word under it, slide to
        // extend, and lift for Copy and Paste: a drag alone scrolls.
        let hold = gtk::GestureLongPress::new();
        hold.set_touch_only(true);
        let app = self.clone();
        hold.connect_pressed(move |_, x, y| app.touch_select(x, y));
        self.view.add_controller(hold);
    }

    /// Opens the link under a tapped point, focusing its pane; false when there is none.
    fn tap_link(self: &Rc<Self>, x: f64, y: f64) -> bool {
        let cell = self.view.cell_at(x, y);
        let pane = {
            let grid = self.grid.borrow();
            if link_url(&grid, cell).is_none() {
                return false;
            }
            grid.pane_at(cell.0, cell.1)
                .map(|pane| (pane.pane_id.clone(), pane.focused))
        };
        let Some((pane_id, focused)) = pane else {
            return false;
        };
        debug(|| format!("tap on a link at cell {cell:?}"));
        if !focused {
            self.request("pane.focus", json!({ "pane_id": pane_id }));
        }
        self.open_link_at(cell)
    }

    /// Starts a touch selection at the word under a held finger.
    fn touch_select(self: &Rc<Self>, x: f64, y: f64) {
        let cell = self.view.cell_at(x, y);
        let (pane_id, focused, bounds) = {
            let grid = self.grid.borrow();
            let Some(pane) = grid.pane_at(cell.0, cell.1) else {
                return;
            };
            let inner = pane.inner_rect;
            (
                pane.pane_id.clone(),
                pane.focused,
                (
                    (inner.x, inner.x + inner.width.saturating_sub(1)),
                    (inner.y, inner.y + inner.height.saturating_sub(1)),
                ),
            )
        };
        if !focused {
            self.request("pane.focus", json!({ "pane_id": pane_id }));
        }
        let word = self.grid.borrow().word_at(cell, bounds.0);
        self.view.set_selection(Some(Selection {
            anchor: word.0,
            head: word.1,
            cols: bounds.0,
        }));
        self.drag.set(Some(DragState::TouchSelect { word, bounds }));
    }

    /// Copy and Paste beside a lifted finger; dismissing it drops the selection.
    fn touch_menu(self: &Rc<Self>, x: f64, y: f64) {
        let text = self
            .view
            .selection()
            .map(|s| self.grid.borrow().text(s.anchor, s.head, s.cols))
            .filter(|text| !text.trim().is_empty());
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let popover = gtk::Popover::builder()
            .child(&row)
            .position(gtk::PositionType::Top)
            .pointing_to(&gdk::Rectangle::new(x as i32, y as i32, 1, 1))
            .build();
        popover.add_css_class("touch-menu");
        if let Some(text) = text {
            let copy = gtk::Button::with_label("Copy");
            let (app, menu) = (self.clone(), popover.downgrade());
            copy.connect_clicked(move |_| {
                app.view.clipboard().set_text(&text);
                app.show_banner("Copied", Some(2));
                if let Some(menu) = menu.upgrade() {
                    menu.popdown();
                }
            });
            row.append(&copy);
        }
        let paste = gtk::Button::with_label("Paste");
        let (app, menu) = (self.clone(), popover.downgrade());
        paste.connect_clicked(move |_| {
            app.paste();
            if let Some(menu) = menu.upgrade() {
                menu.popdown();
            }
        });
        row.append(&paste);
        let app = self.clone();
        popover.connect_closed(move |menu| {
            app.view.set_selection(None);
            menu.unparent();
        });
        popover.set_parent(&self.view);
        popover.popup();
    }

    /// Asks the touch shell for its on-screen keyboard, as a tap on a text field does;
    /// a tap brings it back after it was dismissed.
    fn show_keyboard(&self, event: Option<gdk::Event>) {
        let Some(keyboard) = self.keyboard.borrow().clone() else {
            return;
        };
        self.view.grab_focus();
        // Text input that is already on asks for nothing new, and a keyboard the
        // user dismissed stays down until it is turned on afresh.
        keyboard.focus_out();
        keyboard.focus_in();
        let shown = keyboard.activate_osk(event);
        debug(|| format!("keyboard asked for, accepted={shown}"));
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
        self.fit_sidebar();
        if let Some(conn) = self.conn.borrow().as_ref() {
            conn.resize(&self.geometry());
            return;
        }
        self.connect();
    }

    /// Moves the sidebar between beside the terminal and a drawer over it, after the
    /// current layout pass (a widget can't change parents while it is being allocated).
    fn fit_sidebar(self: &Rc<Self>) {
        let app = self.clone();
        glib::idle_add_local_once(move || {
            app.drawer.fit(app.window.width(), app.window.height());
            app.sync_sidebar_order();
        });
    }

    /// Agents go above the spaces while the sidebar is laid out for fingers.
    fn sync_sidebar_order(self: &Rc<Self>) {
        if self.sidebar.set_agents_first(self.drawer.touch_layout()) {
            self.rebuild_chrome();
        }
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
        if machine.status == EndpointStatus::NotResponding {
            self.stall_label.set_text(&stall_message(&machine.label));
            self.stall.set_reveal_child(true);
        } else {
            self.stall.set_reveal_child(false);
        }
        match &machine.status {
            EndpointStatus::Online | EndpointStatus::NotResponding => self.hide_banner(),
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
                    self.prefs.borrow().toast_delivery,
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
        let (retry, snapshot) = {
            let machines = self.machines.borrow();
            let Some(machine) = machines.iter().find(|m| &m.id == id) else {
                return;
            };
            (retries_on_switch(&machine.status), machine.snapshot.clone())
        };
        if retry {
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
            let fg = self.theme.borrow().foreground;
            let stalled = self.stalled_machines();
            self.sidebar.render(model, &active, &stalled, fg);
        }
        self.update_status();
        self.update_touch_controls();
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
            add.set_focusable(false);
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

    /// The ids of machines that are connected but have stopped answering.
    fn stalled_machines(&self) -> Vec<String> {
        self.machines
            .borrow()
            .iter()
            .filter(|machine| machine.status == EndpointStatus::NotResponding)
            .map(|machine| machine.id.as_str().to_owned())
            .collect()
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
        close.set_focusable(false);
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

        // A long press opens the tab's menu, as a right-click would on a desktop.
        let hold = gtk::GestureLongPress::new();
        hold.set_touch_only(true);
        let (app, id, ws, p) = (
            self.clone(),
            tab.tab_id.clone(),
            tab.workspace_id.clone(),
            pill.downgrade(),
        );
        hold.connect_pressed(move |gesture, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(pill) = p.upgrade() {
                app.tab_menu(app.anchor_rect(&pill), &id, &ws);
            }
        });
        pill.add_controller(hold);
        let right = gtk::GestureClick::new();
        right.set_button(gdk::BUTTON_SECONDARY);
        let (app, id, ws, p) = (
            self.clone(),
            tab.tab_id.clone(),
            tab.workspace_id.clone(),
            pill.downgrade(),
        );
        right.connect_pressed(move |_, _, _, _| {
            if let Some(pill) = p.upgrade() {
                app.tab_menu(app.anchor_rect(&pill), &id, &ws);
            }
        });
        pill.add_controller(right);
        pill
    }

    /// The quick-action strip and the close-pane button, shown in the touch layout
    /// for the focused pane.
    fn update_touch_controls(self: &Rc<Self>) {
        let touch = self.drawer.touch_layout();
        let pane = self.focused_pane_id();
        self.pane_close.set_visible(touch && pane.is_some());
        let agent = match (touch, pane.as_deref(), self.model.borrow().as_ref()) {
            (true, Some(pane), Some(model)) => {
                let active = self
                    .conn
                    .borrow()
                    .as_ref()
                    .map(|conn| conn.active().as_str().to_owned())
                    .unwrap_or_default();
                focused_agent(model, &active, pane)
            }
            _ => None,
        };
        let actions = agent.map(crate::quick::for_agent).unwrap_or_default();
        if *self.quick_actions.borrow() == actions {
            return;
        }
        while let Some(child) = self.quick.first_child() {
            self.quick.remove(&child);
        }
        for action in &actions {
            let button = gtk::Button::with_label(action.label());
            button.set_focusable(false);
            button.set_tooltip_text(Some(action.tooltip()));
            let (app, action) = (self.clone(), *action);
            button.connect_clicked(move |_| app.quick_action(action));
            self.quick.append(&button);
        }
        self.quick.set_visible(!actions.is_empty());
        self.quick_actions.replace(actions);
    }

    fn quick_action(&self, action: crate::quick::QuickAction) {
        let Some(pane_id) = self.focused_pane_id() else {
            return;
        };
        if let Some(conn) = self.conn.borrow().as_ref() {
            conn.pane_input(&pane_id, action.events());
        }
        self.view.set_selection(None);
    }

    fn update_status(&self) {
        let leader = match self.model.borrow().as_ref() {
            Some(model) => model.footer.leader.clone(),
            None => self.keys.borrow().config().primary_prefix_label(),
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
        // Fingers don't use the leader key, so the touch layout leaves its hint out.
        let touch = self.drawer.touch_layout();
        let mut markup = String::from("🌴 oasis");
        match (mode, touch) {
            (Some(mode), _) => {
                markup.push_str(&format!(
                    "  <span background=\"{}\" foreground=\"{}\" weight=\"bold\"> {mode} </span>",
                    theme.accent.css(),
                    theme.sidebar.css()
                ));
                if !touch {
                    markup.push_str(&format!("  {leader}"));
                }
            }
            (None, false) => markup.push_str(&format!(" · leader: {leader}")),
            (None, true) => {}
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
        // The key handler sees keys before GTK's shortcuts, so quit is handled here.
        if key.is(KeyCode::Char('Q'), ctrl_shift) || key.is(KeyCode::Char('q'), ctrl_shift) {
            self.application.quit();
            return glib::Propagation::Stop;
        }
        // ctrl+shift+b shows or puts away the browser panel beside clawmux.
        if key.is(KeyCode::Char('B'), ctrl_shift) || key.is(KeyCode::Char('b'), ctrl_shift) {
            if !self.browser.toggle() {
                self.show_banner("ctrl+click a link to open it in a browser window", Some(3));
            }
            return glib::Propagation::Stop;
        }
        if let Some(zoom) = font_zoom(&key) {
            self.zoom_font(zoom);
            return glib::Propagation::Stop;
        }
        let terminal_key = key.terminal_key();
        // Space and agent navigation spans every machine, as in the TUI; the key engine
        // only sees the active machine, so those bindings are handled here.
        let cross_machine = {
            let keys = self.keys.borrow();
            match route_key(keys.config(), keys.mode(), false, false, &terminal_key).step {
                KeyStep::Binding(KeybindMatch::Action(action)) if spans_machines(action) => {
                    Some(action)
                }
                _ => None,
            }
        };
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
        debug(|| self.describe_key(&key, &outcomes));
        for outcome in outcomes {
            match outcome {
                KeyOutcome::ToPane(_) => self.send_key_to_pane(&key),
                KeyOutcome::Request(_) if cross_machine.is_some() => {}
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
        if let Some(action) = cross_machine {
            self.navigate_across_machines(action);
        }
        glib::Propagation::Stop
    }

    /// Next/previous/numbered space or agent across every machine, in sidebar order,
    /// switching machines when the target lives on another one.
    fn navigate_across_machines(self: &Rc<Self>, action: KeybindAction) {
        let active = match self.conn.borrow().as_ref() {
            Some(conn) => conn.active().as_str().to_owned(),
            None => return,
        };
        let (spaces, agents) = match self.model.borrow().as_ref() {
            Some(model) => (
                model
                    .machines
                    .iter()
                    .flat_map(|machine| {
                        machine
                            .spaces
                            .iter()
                            .map(|space| (machine.id.clone(), space.workspace_id.clone()))
                    })
                    .collect::<Vec<_>>(),
                model
                    .agents
                    .iter()
                    .map(|agent| (agent.endpoint.clone(), agent.pane_id.clone()))
                    .collect::<Vec<_>>(),
            ),
            None => return,
        };
        let workspace = self.focused_workspace_id().unwrap_or_default();
        let pane = self.focused_pane_id().unwrap_or_default();
        let target = match action {
            KeybindAction::PreviousWorkspace | KeybindAction::NextWorkspace => cycle(
                &spaces,
                &(active.clone(), workspace),
                action == KeybindAction::NextWorkspace,
            )
            .map(|t| (t, true)),
            KeybindAction::SwitchWorkspace(index) => spaces.get(index).cloned().map(|t| (t, true)),
            KeybindAction::PreviousAgent | KeybindAction::NextAgent => cycle(
                &agents,
                &(active.clone(), pane),
                action == KeybindAction::NextAgent,
            )
            .map(|t| (t, false)),
            KeybindAction::FocusAgent(index) => agents.get(index).cloned().map(|t| (t, false)),
            _ => None,
        };
        let Some(((endpoint, id), is_space)) = target else {
            return;
        };
        self.switch_machine_str(&endpoint);
        if is_space {
            self.request("workspace.focus", json!({ "workspace_id": id }));
        } else {
            self.request("pane.focus", json!({ "pane_id": id }));
        }
    }

    /// A key for the debug log. Plain typed characters are never logged, only chords and
    /// special keys, with the state the key engine saw.
    fn describe_key(&self, key: &Key, outcomes: &[KeyOutcome]) -> String {
        let chord = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        if !chord && matches!(key.code, KeyCode::Char(_)) {
            return "typed key (not logged)".into();
        }
        let active = self
            .conn
            .borrow()
            .as_ref()
            .map(|conn| conn.active().as_str().to_owned());
        let machines: Vec<String> = self
            .model
            .borrow()
            .as_ref()
            .map(|model| model.machines.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default();
        let focus = gtk::prelude::GtkWindowExt::focus(&self.window).map(|w| w.type_().name());
        format!(
            "key {:?} {:?} active={active:?} model_machines={machines:?} snapshot={} focus={focus:?} -> {outcomes:?}",
            key.code,
            key.modifiers,
            self.snapshot.borrow().is_some(),
        )
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
                    self.rename_pane(&id);
                }
            }
            KeybindAction::ClosePane => {
                if let Some(id) = pane {
                    self.confirm_close_pane(&id);
                }
            }
            KeybindAction::ToggleSidebar => self.drawer.toggle(),
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
        // Switching machines rebuilds the sidebar, so a menu's place is taken first.
        let menu_at = (button == gdk::BUTTON_SECONDARY)
            .then(|| row.map(|row| self.anchor_rect(row)))
            .flatten();
        match target {
            Target::Machine { endpoint } => {
                if button == gdk::BUTTON_SECONDARY {
                    if let Some(rect) = menu_at {
                        self.machine_menu(rect, endpoint);
                    }
                    return;
                }
                self.switch_machine_str(endpoint);
            }
            Target::Space {
                endpoint,
                workspace_id,
            } => {
                self.switch_machine_str(endpoint);
                if button == gdk::BUTTON_SECONDARY {
                    if let Some(rect) = menu_at {
                        self.space_menu(rect, workspace_id);
                    }
                    return;
                }
                self.request("workspace.focus", json!({ "workspace_id": workspace_id }));
                self.drawer.close();
            }
            Target::Agent { endpoint, pane_id } => {
                self.switch_machine_str(endpoint);
                if button == gdk::BUTTON_SECONDARY {
                    if let Some(rect) = menu_at {
                        self.agent_menu(rect, pane_id);
                    }
                    return;
                }
                self.request("pane.focus", json!({ "pane_id": pane_id }));
                self.drawer.close();
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

    /// Like the TUI on focus gain: machines still connecting or reconnecting retry now.
    fn retry_waiting_machines(&self) {
        let Some(conn) = self.conn.borrow().clone() else {
            return;
        };
        for machine in self.machines.borrow().iter() {
            if matches!(
                machine.status,
                EndpointStatus::Connecting | EndpointStatus::Reconnecting(_)
            ) {
                conn.retry(&machine.id);
            }
        }
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

    fn rename_pane(self: &Rc<Self>, pane_id: &str) {
        let current = self
            .snapshot
            .borrow()
            .as_ref()
            .and_then(|s| s.panes.iter().find(|p| p.pane_id == pane_id))
            .and_then(|p| p.label.clone())
            .unwrap_or_default();
        let (app, id) = (self.clone(), pane_id.to_owned());
        self.prompt("Rename pane", &current, move |label| {
            app.request("pane.rename", json!({ "pane_id": id, "label": label }));
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

    /// Asks Yes or No before closing something, with buttons big enough for a finger.
    fn confirm(self: &Rc<Self>, message: &str, action: impl FnOnce() + 'static) {
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("clawmux")
            .default_width(380)
            .resizable(false)
            .build();
        dialog.add_css_class("clawmux-confirm");
        let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
        body.set_margin_top(22);
        body.set_margin_bottom(20);
        body.set_margin_start(22);
        body.set_margin_end(22);
        let label = gtk::Label::new(Some(message));
        label.set_wrap(true);
        label.set_xalign(0.0);
        label.add_css_class("confirm-message");
        body.append(&label);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        buttons.set_homogeneous(true);
        let no = gtk::Button::with_label("No");
        let yes = gtk::Button::with_label("Yes");
        yes.add_css_class("destructive-action");
        buttons.append(&no);
        buttons.append(&yes);
        body.append(&buttons);
        dialog.set_child(Some(&body));
        let action = Rc::new(Cell::new(Some(action)));
        let d = dialog.clone();
        yes.connect_clicked(move |_| {
            if let Some(action) = action.take() {
                action();
            }
            d.close();
        });
        let d = dialog.clone();
        no.connect_clicked(move |_| d.close());
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
        let app = self.clone();
        dialog.connect_close_request(move |_| {
            app.view.grab_focus();
            glib::Propagation::Proceed
        });
        dialog.set_default_widget(Some(&yes));
        dialog.present();
        yes.grab_focus();
    }

    fn confirm_close_pane(self: &Rc<Self>, pane_id: &str) {
        let (app, id) = (self.clone(), pane_id.to_owned());
        self.confirm("Close this pane?", move || {
            app.request("pane.close", json!({ "pane_id": id }));
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
            Some(DragState::TouchSelect { word, bounds }) => {
                let head = self.view.cell_at(x, y);
                let head = (
                    head.0.clamp(bounds.0 .0, bounds.0 .1),
                    head.1.clamp(bounds.1 .0, bounds.1 .1),
                );
                let (anchor, head) = if (head.1, head.0) < (word.0 .1, word.0 .0) {
                    (word.1, head)
                } else if (head.1, head.0) > (word.1 .1, word.1 .0) {
                    (word.0, head)
                } else {
                    word
                };
                self.view.set_selection(Some(Selection {
                    anchor,
                    head,
                    cols: bounds.0,
                }));
            }
            Some(DragState::Finger { .. }) | None => {}
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
            Some(DragState::Finger { .. } | DragState::TouchSelect { .. }) | None => {}
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

    /// Opens the link under a cell, an OSC 8 hyperlink or a plain URL in the text, in a
    /// new window of the user's default browser.
    fn open_link_at(self: &Rc<Self>, cell: (u16, u16)) -> bool {
        let url = link_url(&self.grid.borrow(), cell);
        let Some(url) = url else { return false };
        let scheme = url
            .split(':')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        // Browsers drop their own pages (chrome://settings) when another app opens
        // them, so those are copied for the address bar.
        if links::browser_internal_page(&url) {
            self.view.clipboard().set_text(&url);
            self.show_banner(
                &format!("copied {url}: browsers open their own pages only from the address bar, so paste it there"),
                Some(5),
            );
            return true;
        }
        // Web pages open in the browser window; other schemes (mailto, vscode, …)
        // go to the app the desktop has for them.
        let opened = if matches!(scheme.as_str(), "http" | "https" | "about") {
            self.browser.open(&url)
        } else {
            gio::AppInfo::launch_default_for_uri(&url, gio::AppLaunchContext::NONE)
                .map_err(|error| std::io::Error::other(error.to_string()))
        };
        if let Err(error) = opened {
            self.show_banner(&format!("could not open {url}: {error}"), Some(5));
        }
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

/// The link under `cell`: its OSC 8 hyperlink, else a link written in the pane's
/// text, followed across the rows a long one wrapped over.
fn link_url(grid: &Grid, cell: (u16, u16)) -> Option<String> {
    grid.cell(cell.0, cell.1)
        .and_then(|c| c.hyperlink)
        .and_then(|index| grid.hyperlinks.get(index as usize).cloned())
        .filter(|target| links::openable(target))
        .or_else(|| url_at(grid, cell))
}

/// A link written in the text of the pane under `cell`, for apps that print links
/// without OSC 8.
fn url_at(grid: &Grid, cell: (u16, u16)) -> Option<String> {
    let (left, right) = grid
        .pane_at(cell.0, cell.1)
        .map(|pane| {
            let r = pane.inner_rect;
            (r.x, (r.x + r.width).saturating_sub(1))
        })
        .unwrap_or((0, grid.width.saturating_sub(1)));
    let right = right.min(grid.width.saturating_sub(1));
    let rows: Vec<links::Row> = (0..grid.height)
        .map(|row| {
            let mut chars = Vec::new();
            for col in left..=right {
                let Some(c) = grid.cell(col, row) else { break };
                if c.skip {
                    continue;
                }
                if c.symbol.is_empty() {
                    chars.push((' ', col));
                } else {
                    chars.extend(c.symbol.chars().map(|ch| (ch, col)));
                }
            }
            chars
        })
        .collect();
    links::link_in_text(&rows, right, usize::from(cell.1), cell.0)
}

/// One entry of a sidebar or tab menu.
type MenuItem = (&'static str, Box<dyn Fn()>);

/// The agent running in the focused pane of the active machine, if any.
fn focused_agent(
    model: &SidebarModel,
    active: &str,
    pane_id: &str,
) -> Option<oasis_core::embed::sidebar::Agent> {
    model
        .agents
        .iter()
        .find(|agent| agent.endpoint == active && agent.pane_id == pane_id)
        .and_then(|agent| agent.agent)
}

/// The bar across the terminal while the active machine has stopped answering.
fn stall_message(machine: &str) -> String {
    format!(
        "◌ {machine} isn't responding · the connection has stalled · what you type is sent once it answers"
    )
}

/// Whether switching to a machine should retry it now: not when it is online, nor when
/// it is still connected and only waiting on a stalled network.
fn retries_on_switch(status: &EndpointStatus) -> bool {
    !matches!(
        status,
        EndpointStatus::Online | EndpointStatus::NotResponding
    )
}

/// Bindings that move between spaces or agents, which span every connected machine.
fn spans_machines(action: KeybindAction) -> bool {
    matches!(
        action,
        KeybindAction::PreviousWorkspace
            | KeybindAction::NextWorkspace
            | KeybindAction::SwitchWorkspace(_)
            | KeybindAction::PreviousAgent
            | KeybindAction::NextAgent
            | KeybindAction::FocusAgent(_)
    )
}

/// The entry after (or before) `current`, wrapping; the first one when `current` is absent.
fn cycle<T: Clone + PartialEq>(items: &[T], current: &T, forward: bool) -> Option<T> {
    if items.is_empty() {
        return None;
    }
    let next = match items.iter().position(|item| item == current) {
        Some(index) if forward => (index + 1) % items.len(),
        Some(index) => (index + items.len() - 1) % items.len(),
        None => 0,
    };
    items.get(next).cloned()
}

/// Debug logging: `CLAWMUX_DEBUG=1` prints to stderr; creating
/// ~/.local/state/clawmux/debug turns on a log at ~/.local/state/clawmux/debug.log,
/// so it can be enabled for an app started from a launcher.
fn debug(message: impl FnOnce() -> String) {
    use std::io::Write as _;
    use std::sync::OnceLock;
    static LOG: OnceLock<Option<std::sync::Mutex<std::fs::File>>> = OnceLock::new();
    let log = LOG.get_or_init(|| {
        let dir = std::env::var_os("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| std::path::Path::new(&home).join(".local/state"))
            })?
            .join("clawmux");
        if !dir.join("debug").exists() {
            return None;
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("debug.log"))
            .ok()
            .map(std::sync::Mutex::new)
    });
    let to_stderr = std::env::var_os("CLAWMUX_DEBUG").is_some();
    if !to_stderr && log.is_none() {
        return;
    }
    let line = message();
    if to_stderr {
        eprintln!("clawmux: {line}");
    }
    if let Some(file) = log {
        if let Ok(mut file) = file.lock() {
            let _ = writeln!(file, "{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid_with_row(text: &str) -> Grid {
        grid_with_rows(&[text])
    }

    /// A grid of `rows`, each padded with blanks to the longest.
    fn grid_with_rows(rows: &[&str]) -> Grid {
        use oasis_core::embed::wire::{CellData, FrameData, PaneSurfaceFrame};
        let width = rows
            .iter()
            .map(|row| row.chars().count())
            .max()
            .unwrap_or(0);
        let cells: Vec<CellData> = rows
            .iter()
            .flat_map(|row| row.chars().chain(std::iter::repeat(' ')).take(width))
            .map(|c| CellData {
                symbol: c.into(),
                fg: 0,
                bg: 0,
                modifier: 0,
                skip: false,
                hyperlink: None,
            })
            .collect();
        let mut grid = Grid::new();
        grid.set_full(PaneSurfaceFrame {
            boot_id: "b".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: FrameData {
                width: width as u16,
                height: rows.len() as u16,
                cells,
                cursor: None,
                hyperlinks: vec![],
                graphics: vec![],
            },
            panes: vec![],
            splits: vec![],
            popup: None,
            graphics: Default::default(),
        });
        grid
    }

    #[test]
    fn browser_pages_and_wrapped_urls_are_found() {
        let grid = grid_with_row("open chrome://settings/performance now");
        assert_eq!(
            url_at(&grid, (9, 0)).as_deref(),
            Some("chrome://settings/performance")
        );
        let grid = grid_with_rows(&["  https://example.org/a-long-path-th", "  at-wraps?q=1"]);
        let whole = Some("https://example.org/a-long-path-that-wraps?q=1");
        assert_eq!(url_at(&grid, (10, 0)).as_deref(), whole);
        assert_eq!(url_at(&grid, (5, 1)).as_deref(), whole);
    }

    #[test]
    fn plain_urls_are_found_under_the_clicked_cell() {
        let grid = grid_with_row("see https://example.com/a?b=1. and http://x.io");
        assert_eq!(
            url_at(&grid, (4, 0)).as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(
            url_at(&grid, (20, 0)).as_deref(),
            Some("https://example.com/a?b=1")
        );
        assert_eq!(url_at(&grid, (2, 0)), None);
        assert_eq!(url_at(&grid, (40, 0)).as_deref(), Some("http://x.io"));
    }

    #[test]
    fn a_tapped_cell_links_to_its_osc8_target_before_its_text() {
        let mut grid = grid_with_row("docs here https://plain.example");
        grid.hyperlinks = vec!["https://osc8.example/docs".into()];
        for col in 0..4 {
            if let Some(cell) = grid.cells.get_mut(col) {
                cell.hyperlink = Some(0);
            }
        }
        assert_eq!(
            link_url(&grid, (1, 0)).as_deref(),
            Some("https://osc8.example/docs")
        );
        assert_eq!(
            link_url(&grid, (12, 0)).as_deref(),
            Some("https://plain.example")
        );
        assert_eq!(link_url(&grid, (6, 0)), None, "a tap off any link");
        assert_eq!(link_url(&grid, (99, 0)), None, "a tap past the row");
    }

    #[test]
    fn quick_actions_follow_the_agent_in_the_focused_pane_of_the_active_machine() {
        use oasis_core::embed::sidebar::{Agent, AgentEntry};
        let entry = |endpoint: &str, pane: &str, agent| AgentEntry {
            endpoint: endpoint.into(),
            machine_label: endpoint.into(),
            pane_id: pane.into(),
            workspace_id: "w1".into(),
            status: AgentStatus::Working,
            mode: None,
            agent,
            focused: false,
            stale: false,
            rows: vec![],
        };
        let mut model = SidebarModel::default();
        model.agents = vec![
            entry("local", "p1", Some(Agent::Claude)),
            entry("ssh:box", "p1", Some(Agent::Codex)),
            entry("local", "p2", None),
        ];
        assert_eq!(focused_agent(&model, "local", "p1"), Some(Agent::Claude));
        assert_eq!(focused_agent(&model, "ssh:box", "p1"), Some(Agent::Codex));
        assert_eq!(
            focused_agent(&model, "local", "p2"),
            None,
            "an unknown tool"
        );
        assert_eq!(focused_agent(&model, "local", "p9"), None, "a plain shell");
    }

    #[test]
    fn a_stalled_machine_is_named_in_its_bar_and_not_retried() {
        let message = stall_message("server");
        assert!(message.starts_with("◌ server isn't responding"));
        assert!(message.ends_with("what you type is sent once it answers"));
        assert!(!retries_on_switch(&EndpointStatus::NotResponding));
        assert!(!retries_on_switch(&EndpointStatus::Online));
        assert!(retries_on_switch(&EndpointStatus::Connecting));
        assert!(retries_on_switch(&EndpointStatus::Reconnecting(
            "gone".into()
        )));
        assert!(retries_on_switch(&EndpointStatus::Attention(
            "login".into()
        )));
    }

    #[test]
    fn cycle_wraps_across_entries_from_every_machine() {
        let items = vec![("local", "w4"), ("ssh:a", "w1"), ("ssh:a", "w2")];
        assert_eq!(cycle(&items, &("local", "w4"), true), Some(("ssh:a", "w1")));
        assert_eq!(
            cycle(&items, &("local", "w4"), false),
            Some(("ssh:a", "w2"))
        );
        assert_eq!(cycle(&items, &("ssh:a", "w2"), true), Some(("local", "w4")));
        assert_eq!(cycle(&items, &("gone", "w9"), true), Some(("local", "w4")));
        assert_eq!(cycle::<(&str, &str)>(&[], &("local", "w4"), true), None);
    }
}
