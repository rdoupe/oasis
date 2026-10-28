//! Where the sidebar sits. A wide window keeps it beside the terminal; a portrait or
//! narrow one (a tablet) turns it into a drawer over the terminal, closed by default, so
//! the terminal keeps the whole width. The drawer opens from the ☰ button or a swipe in
//! from the left edge, and closes with a swipe left, a tap beside it, or a choice in it.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;

/// Narrower than this, or taller than wide, and the sidebar becomes a drawer.
const DRAWER_BELOW_WIDTH: i32 = 900;
/// A touch that starts this close to the left edge can pull the drawer open.
const EDGE: f64 = 28.0;
/// How far a swipe travels before it opens or closes the drawer.
const SWIPE: f64 = 60.0;

/// Whether a window of this size shows the sidebar as a drawer.
pub fn is_drawer(width: i32, height: i32) -> bool {
    width > 0 && (width < DRAWER_BELOW_WIDTH || height > width)
}

/// Whether a horizontal swipe of `dx`, `dy` counts as one toward `sign` (1 right, -1 left).
pub fn is_swipe(dx: f64, dy: f64, sign: f64) -> bool {
    dx * sign >= SWIPE && dx.abs() > dy.abs()
}

/// Whether the sidebar is laid out for fingers: as a drawer, or on a touchscreen.
pub fn is_touch_layout(drawer: bool, touch: bool) -> bool {
    drawer || touch
}

pub struct Drawer {
    /// What the window holds: the inline sidebar and the main area, with the drawer above.
    pub root: gtk::Overlay,
    sidebar: gtk::Widget,
    inline: gtk::Box,
    revealer: gtk::Revealer,
    scrim: gtk::Box,
    /// The ☰ button; the window puts it at the start of the tab bar.
    pub button: gtk::Button,
    drawer: Cell<bool>,
    /// Whether the inline sidebar is shown in a wide window (the toggle keybind).
    inline_shown: Cell<bool>,
    /// A touchscreen keeps the big touch targets in either orientation.
    touch: Cell<bool>,
}

impl Drawer {
    pub fn new(sidebar: &impl IsA<gtk::Widget>, main: &impl IsA<gtk::Widget>) -> Rc<Self> {
        let sidebar = sidebar.clone().upcast::<gtk::Widget>();
        let inline = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        inline.append(&sidebar);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.append(&inline);
        row.append(main);

        let scrim = gtk::Box::new(gtk::Orientation::Vertical, 0);
        scrim.add_css_class("clawmux-scrim");
        scrim.set_hexpand(true);
        scrim.set_vexpand(true);
        scrim.set_visible(false);
        let revealer = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideRight)
            .transition_duration(160)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Fill)
            .build();
        revealer.add_css_class("clawmux-drawer");
        // A closed revealer still covers its strip; let touches reach the terminal.
        revealer.set_can_target(false);

        let root = gtk::Overlay::new();
        root.set_child(Some(&row));
        root.add_overlay(&scrim);
        root.add_overlay(&revealer);

        let button = gtk::Button::with_label("☰");
        button.add_css_class("sidebar-toggle");
        button.set_focusable(false);
        button.set_tooltip_text(Some("Spaces and agents"));

        let drawer = Rc::new(Self {
            root,
            sidebar,
            inline,
            revealer,
            scrim,
            button,
            drawer: Cell::new(false),
            inline_shown: Cell::new(true),
            touch: Cell::new(false),
        });
        drawer.wire();
        drawer
    }

    fn wire(self: &Rc<Self>) {
        let this = Rc::downgrade(self);
        self.button.connect_clicked(move |_| {
            if let Some(this) = this.upgrade() {
                this.toggle();
            }
        });

        let tap = gtk::GestureClick::new();
        let this = Rc::downgrade(self);
        tap.connect_released(move |gesture, _, _, _| {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            if let Some(this) = this.upgrade() {
                this.close();
            }
        });
        self.scrim.add_controller(tap);

        // Swipe in from the left edge: only touches that start at the edge are claimed,
        // so every other touch still reaches the terminal.
        let edge = gtk::GestureDrag::new();
        edge.set_touch_only(true);
        edge.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = Rc::downgrade(self);
        edge.connect_drag_begin(move |gesture, x, _| {
            let Some(this) = this.upgrade() else { return };
            if this.drawer.get() && !this.is_open() && x <= EDGE {
                gesture.set_state(gtk::EventSequenceState::Claimed);
            } else {
                gesture.set_state(gtk::EventSequenceState::Denied);
            }
        });
        let this = Rc::downgrade(self);
        edge.connect_drag_update(move |gesture, dx, dy| {
            if let Some(this) = this.upgrade() {
                if is_swipe(dx, dy, 1.0) && !this.is_open() {
                    this.open();
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                }
            }
        });
        self.root.add_controller(edge);

        // Swipe the open drawer left to close it. Taps on its rows are left alone until
        // the touch has clearly moved sideways.
        let back = gtk::GestureDrag::new();
        back.set_touch_only(true);
        back.set_propagation_phase(gtk::PropagationPhase::Capture);
        let this = Rc::downgrade(self);
        back.connect_drag_update(move |gesture, dx, dy| {
            if let Some(this) = this.upgrade() {
                if is_swipe(dx, dy, -1.0) && this.is_open() {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    this.close();
                }
            }
        });
        self.revealer.add_controller(back);
    }

    /// Fits the sidebar to the window's size; call it whenever the window resizes.
    pub fn fit(&self, width: i32, height: i32) {
        let drawer = is_drawer(width, height);
        if drawer == self.drawer.get() && self.sidebar.parent().is_some() {
            return;
        }
        self.drawer.set(drawer);
        self.sidebar.unparent();
        self.style_for_touch(is_touch_layout(drawer, self.touch.get()));
        if drawer {
            self.revealer.set_child(Some(&self.sidebar));
            self.sidebar.set_visible(true);
            self.set_open(false);
        } else {
            self.revealer.set_child(None::<&gtk::Widget>);
            self.inline.append(&self.sidebar);
            self.sidebar.set_visible(self.inline_shown.get());
            self.set_open(false);
        }
    }

    pub fn set_touch(&self, touch: bool) {
        self.touch.set(touch);
        self.style_for_touch(is_touch_layout(self.drawer.get(), touch));
    }

    /// Whether the sidebar is now laid out for fingers (see `is_touch_layout`).
    pub fn touch_layout(&self) -> bool {
        is_touch_layout(self.drawer.get(), self.touch.get())
    }

    fn style_for_touch(&self, touch: bool) {
        if touch {
            self.root.add_css_class("touch");
        } else {
            self.root.remove_css_class("touch");
        }
    }

    pub fn is_open(&self) -> bool {
        self.revealer.reveals_child()
    }

    pub fn open(&self) {
        if self.drawer.get() {
            self.set_open(true);
        }
    }

    /// Closes the drawer; a wide window's sidebar is left as it is.
    pub fn close(&self) {
        if self.drawer.get() {
            self.set_open(false);
        }
    }

    /// The ☰ button and the toggle-sidebar keybind.
    pub fn toggle(&self) {
        if self.drawer.get() {
            self.set_open(!self.is_open());
        } else {
            let shown = !self.sidebar.is_visible();
            self.inline_shown.set(shown);
            self.sidebar.set_visible(shown);
        }
    }

    fn set_open(&self, open: bool) {
        self.revealer.set_reveal_child(open);
        self.revealer.set_can_target(open);
        self.scrim.set_visible(open);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portrait_and_narrow_windows_use_the_drawer() {
        assert!(is_drawer(1200, 1920), "portrait tablet");
        assert!(is_drawer(800, 600), "narrow landscape");
        assert!(!is_drawer(1920, 1200), "landscape tablet");
        assert!(!is_drawer(1400, 860), "desktop window");
        assert!(!is_drawer(0, 0), "not yet sized");
    }

    #[test]
    fn a_drawer_or_a_touchscreen_lays_the_sidebar_out_for_fingers() {
        assert!(is_touch_layout(true, false), "portrait drawer");
        assert!(is_touch_layout(false, true), "landscape touchscreen");
        assert!(!is_touch_layout(false, false), "wide desktop window");
    }

    #[test]
    fn a_swipe_must_be_long_and_mostly_sideways() {
        assert!(is_swipe(80.0, 10.0, 1.0));
        assert!(is_swipe(-80.0, 10.0, -1.0));
        assert!(!is_swipe(80.0, 10.0, -1.0), "wrong direction");
        assert!(!is_swipe(30.0, 0.0, 1.0), "too short");
        assert!(!is_swipe(70.0, 120.0, 1.0), "a scroll, not a swipe");
    }
}
