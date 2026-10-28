//! Touchscreen support: whether the seat has one, the on-screen keyboard (the terminal
//! asks for text input, which a touch shell answers by showing its keyboard), and the
//! sums behind pinch-to-zoom and finger scrolling.

use gtk::gdk;
use gtk::prelude::*;

use crate::theme::{MAX_FONT_SIZE, MIN_FONT_SIZE};

/// A touch that moves less than this is a tap, not a scroll.
pub const TAP_SLOP: f64 = 8.0;

/// Whether this display's seat has a touchscreen.
pub fn has_touchscreen() -> bool {
    gdk::Display::default()
        .and_then(|display| display.default_seat())
        .is_some_and(|seat| seat.capabilities().contains(gdk::SeatCapabilities::TOUCH))
}

/// Whether the event that started a gesture came from a finger.
pub fn from_finger(gesture: &impl IsA<gtk::EventController>) -> bool {
    gesture.current_event().is_some_and(|event| {
        matches!(
            event.event_type(),
            gdk::EventType::TouchBegin
                | gdk::EventType::TouchUpdate
                | gdk::EventType::TouchEnd
                | gdk::EventType::TouchCancel
        ) || event
            .device()
            .is_some_and(|device| device.source() == gdk::InputSource::Touchscreen)
    })
}

/// The font size a pinch of `scale` makes of `base`, in half points.
pub fn pinched_size(base: f64, scale: f64) -> f64 {
    ((base * scale * 2.0).round() / 2.0).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
}

/// Lines to scroll for a finger moving from `from_y` to `to_y`: a finger moving up
/// scrolls toward newer output (positive), as a touch list does.
pub fn scrolled_lines(from_y: f64, to_y: f64, line_height: f64) -> f64 {
    if line_height <= 0.0 {
        return 0.0;
    }
    (from_y - to_y) / line_height
}

/// An input method context that asks the compositor for text input whenever the
/// terminal has focus, so a touch shell shows its on-screen keyboard. Text typed on
/// it arrives through `commit`.
pub fn keyboard_for(
    view: &impl IsA<gtk::Widget>,
    commit: impl Fn(&str) + 'static,
) -> gtk::IMMulticontext {
    let im = gtk::IMMulticontext::new();
    im.set_client_widget(Some(view));
    im.set_input_purpose(gtk::InputPurpose::Terminal);
    im.set_input_hints(gtk::InputHints::NO_SPELLCHECK | gtk::InputHints::NO_EMOJI);
    im.connect_commit(move |_, text| commit(text));
    let focus = gtk::EventControllerFocus::new();
    let context = im.clone();
    focus.connect_enter(move |_| context.focus_in());
    let context = im.clone();
    focus.connect_leave(move |_| context.focus_out());
    view.add_controller(focus);
    if view.has_focus() {
        im.focus_in();
    }
    im
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pinch_scales_the_font_in_half_points_within_bounds() {
        assert_eq!(pinched_size(11.0, 1.0), 11.0);
        assert_eq!(pinched_size(11.0, 1.5), 16.5);
        assert_eq!(pinched_size(11.0, 1.12), 12.5);
        assert_eq!(pinched_size(11.0, 0.1), MIN_FONT_SIZE);
        assert_eq!(pinched_size(11.0, 10.0), MAX_FONT_SIZE);
    }

    #[test]
    fn a_finger_moving_up_scrolls_toward_newer_output() {
        assert_eq!(scrolled_lines(100.0, 60.0, 20.0), 2.0);
        assert_eq!(scrolled_lines(60.0, 100.0, 20.0), -2.0);
        assert_eq!(scrolled_lines(60.0, 100.0, 0.0), 0.0);
    }
}
