//! The mouse rules shared by the TUI and embedders: when a press focuses,
//! selects, opens a link or goes to the pane's app, how double clicks are
//! recognized, when a plain click opens the link under it, and how far the
//! wheel scrolls. Coordinates are cells relative to the pane's inner area.
//!
//! The engine never starts a timer. A plain click that may open a link
//! returns a deadline; the embedder calls [`MouseEngine::tick`] once it
//! passes.

use std::time::{Duration, Instant};

/// How long a plain click waits before opening its link, so the first click
/// of a double-click never opens anything.
pub const PLAIN_LINK_CLICK_DELAY: Duration = Duration::from_millis(350);

/// The longest gap between two presses that still makes a double click.
pub const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(350);

/// A press position in a pane, with its time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClickSpot {
    pub pane_id: String,
    pub row: u16,
    pub col: u16,
    pub at: Instant,
}

/// Whether `next` continues a click sequence started by `previous`: same
/// pane, within [`MULTI_CLICK_INTERVAL`], and at most one cell away.
pub fn is_repeat_click(previous: &ClickSpot, next: &ClickSpot) -> bool {
    previous.pane_id == next.pane_id
        && next.at.saturating_duration_since(previous.at) <= MULTI_CLICK_INTERVAL
        && previous.row.abs_diff(next.row) <= 1
        && previous.col.abs_diff(next.col) <= 1
}

/// Whether a left release lands on the cell its press went down on, which a
/// plain link click requires. `dragged` is true when the gesture became a
/// selection drag.
pub fn released_in_place(press: (u16, u16), release: Option<(u16, u16)>, dragged: bool) -> bool {
    !dragged && release == Some(press)
}

/// The scrollback offset a wheel step moves to, `lines` per step, clamped to
/// the pane's history.
pub fn wheel_scroll_offset(metrics: crate::pane::ScrollMetrics, up: bool, lines: usize) -> usize {
    if up {
        metrics
            .offset_from_bottom
            .saturating_add(lines)
            .min(metrics.max_offset_from_bottom)
    } else {
        metrics.offset_from_bottom.saturating_sub(lines)
    }
}

/// A left press on a pane, as the embedder sees it.
#[derive(Clone, Debug)]
pub struct PanePress {
    pub pane_id: String,
    pub row: u16,
    pub col: u16,
    /// The pane is the focused one.
    pub focused: bool,
    /// The pane's app reports the mouse, so the press also goes to it.
    pub mouse_reporting: bool,
    /// Ctrl is held: open the link under the press at once.
    pub ctrl: bool,
    /// No modifier is held.
    pub plain: bool,
    /// The `click_opens_links` setting.
    pub click_opens_links: bool,
}

/// What a mouse event does, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MouseOutcome {
    /// Focus this pane (`pane.focus`).
    FocusPane(String),
    /// Send the event to the pane's app.
    PassThrough,
    /// Ask the server to open the link or path under this cell
    /// (`pane.link.activate`).
    ActivateLink { pane_id: String, row: u16, col: u16 },
    /// Start a selection anchored here.
    BeginSelection { pane_id: String, row: u16, col: u16 },
    /// Select the word under this cell (a double click).
    SelectWord { pane_id: String, row: u16, col: u16 },
    /// A plain click may open its link once this deadline passes; call
    /// [`MouseEngine::tick`] then.
    LinkPending(Instant),
}

#[derive(Clone, Debug)]
struct LinkPress {
    pane_id: String,
    row: u16,
    col: u16,
}

/// Mouse state for one embedder window.
#[derive(Default)]
pub struct MouseEngine {
    last_click: Option<ClickSpot>,
    link_press: Option<LinkPress>,
    link_pending: Option<(LinkPress, Instant)>,
}

impl MouseEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// A left press on a pane.
    pub fn press(&mut self, press: PanePress, now: Instant) -> Vec<MouseOutcome> {
        self.link_press = None;
        self.link_pending = None;
        let mut outcomes = Vec::new();
        if press.ctrl {
            self.last_click = None;
            outcomes.push(MouseOutcome::ActivateLink {
                pane_id: press.pane_id,
                row: press.row,
                col: press.col,
            });
            return outcomes;
        }
        let may_open = press.plain && press.click_opens_links && press.focused;
        if !press.focused {
            outcomes.push(MouseOutcome::FocusPane(press.pane_id.clone()));
        }
        if press.mouse_reporting {
            if may_open {
                self.link_press = Some(LinkPress {
                    pane_id: press.pane_id,
                    row: press.row,
                    col: press.col,
                });
            }
            outcomes.push(MouseOutcome::PassThrough);
            return outcomes;
        }
        let spot = ClickSpot {
            pane_id: press.pane_id.clone(),
            row: press.row,
            col: press.col,
            at: now,
        };
        let previous = self.last_click.take();
        if press.plain
            && previous
                .as_ref()
                .is_some_and(|previous| is_repeat_click(previous, &spot))
        {
            outcomes.push(MouseOutcome::SelectWord {
                pane_id: press.pane_id,
                row: press.row,
                col: press.col,
            });
            return outcomes;
        }
        if press.plain {
            self.last_click = Some(spot);
            if may_open {
                self.link_press = Some(LinkPress {
                    pane_id: press.pane_id.clone(),
                    row: press.row,
                    col: press.col,
                });
            }
        }
        outcomes.push(MouseOutcome::BeginSelection {
            pane_id: press.pane_id,
            row: press.row,
            col: press.col,
        });
        outcomes
    }

    /// The pointer moved with the button down; a drag never opens a link.
    pub fn drag(&mut self) {
        self.link_press = None;
    }

    /// The left button came up at `cell` in `pane_id`. `dragged` is true when
    /// the gesture became a selection. Returns the link deadline if one was
    /// armed.
    pub fn release(
        &mut self,
        pane_id: &str,
        cell: Option<(u16, u16)>,
        dragged: bool,
        now: Instant,
    ) -> Option<MouseOutcome> {
        let press = self.link_press.take()?;
        let in_place =
            press.pane_id == pane_id && released_in_place((press.row, press.col), cell, dragged);
        if dragged {
            self.last_click = None;
        }
        in_place.then(|| {
            let deadline = now + PLAIN_LINK_CLICK_DELAY;
            self.link_pending = Some((press, deadline));
            MouseOutcome::LinkPending(deadline)
        })
    }

    /// Typing, a mode change or focus loss cancels a waiting link click.
    pub fn cancel_link(&mut self) {
        self.link_press = None;
        self.link_pending = None;
    }

    /// When the waiting link click is due, if any.
    pub fn deadline(&self) -> Option<Instant> {
        self.link_pending.as_ref().map(|(_, deadline)| *deadline)
    }

    /// Opens the waiting link click once its deadline has passed.
    pub fn tick(&mut self, now: Instant) -> Option<MouseOutcome> {
        if self
            .link_pending
            .as_ref()
            .is_none_or(|(_, deadline)| now < *deadline)
        {
            return None;
        }
        let (press, _) = self.link_pending.take()?;
        Some(MouseOutcome::ActivateLink {
            pane_id: press.pane_id,
            row: press.row,
            col: press.col,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(focused: bool, mouse_reporting: bool) -> PanePress {
        PanePress {
            pane_id: "p1".into(),
            row: 2,
            col: 5,
            focused,
            mouse_reporting,
            ctrl: false,
            plain: true,
            click_opens_links: true,
        }
    }

    #[test]
    fn plain_click_on_the_focused_pane_opens_its_link_after_the_delay() {
        let mut engine = MouseEngine::new();
        let now = Instant::now();
        let outcomes = engine.press(press(true, false), now);
        assert!(matches!(
            outcomes.as_slice(),
            [MouseOutcome::BeginSelection { .. }]
        ));
        let pending = engine.release("p1", Some((2, 5)), false, now);
        let Some(MouseOutcome::LinkPending(deadline)) = pending else {
            panic!("armed link: {pending:?}");
        };
        assert_eq!(engine.deadline(), Some(deadline));
        assert!(engine.tick(deadline - Duration::from_millis(1)).is_none());
        assert!(matches!(
            engine.tick(deadline),
            Some(MouseOutcome::ActivateLink { row: 2, col: 5, .. })
        ));
        assert!(engine.tick(deadline).is_none(), "one shot");
    }

    #[test]
    fn a_focusing_click_or_a_drag_never_opens_a_link() {
        let mut engine = MouseEngine::new();
        let now = Instant::now();
        let outcomes = engine.press(press(false, false), now);
        assert_eq!(outcomes[0], MouseOutcome::FocusPane("p1".into()));
        assert!(engine.release("p1", Some((2, 5)), false, now).is_none());

        engine.press(press(true, false), now);
        engine.drag();
        assert!(engine.release("p1", Some((2, 5)), true, now).is_none());
    }

    #[test]
    fn a_second_press_selects_a_word_and_cancels_the_link() {
        let mut engine = MouseEngine::new();
        let now = Instant::now();
        engine.press(press(true, false), now);
        engine.release("p1", Some((2, 5)), false, now);
        let second = engine.press(press(true, false), now + Duration::from_millis(100));
        assert!(matches!(
            second.as_slice(),
            [MouseOutcome::SelectWord { .. }]
        ));
        assert!(engine.deadline().is_none());
    }

    #[test]
    fn mouse_reporting_panes_get_the_click_and_still_open_links() {
        let mut engine = MouseEngine::new();
        let now = Instant::now();
        let outcomes = engine.press(press(true, true), now);
        assert_eq!(outcomes, vec![MouseOutcome::PassThrough]);
        assert!(matches!(
            engine.release("p1", Some((2, 5)), false, now),
            Some(MouseOutcome::LinkPending(_))
        ));
    }

    #[test]
    fn ctrl_click_opens_at_once() {
        let mut engine = MouseEngine::new();
        let outcomes = engine.press(
            PanePress {
                ctrl: true,
                plain: false,
                ..press(false, true)
            },
            Instant::now(),
        );
        assert!(matches!(
            outcomes.as_slice(),
            [MouseOutcome::ActivateLink { .. }]
        ));
    }

    #[test]
    fn wheel_steps_clamp_to_history() {
        let metrics = crate::pane::ScrollMetrics {
            offset_from_bottom: 4,
            max_offset_from_bottom: 6,
            viewport_rows: 20,
        };
        assert_eq!(wheel_scroll_offset(metrics, true, 3), 6);
        assert_eq!(wheel_scroll_offset(metrics, false, 3), 1);
        assert_eq!(wheel_scroll_offset(metrics, false, 9), 0);
    }

    #[test]
    fn repeat_clicks_need_the_same_pane_nearby_and_soon() {
        let at = Instant::now();
        let first = ClickSpot {
            pane_id: "p1".into(),
            row: 3,
            col: 3,
            at,
        };
        let near = ClickSpot {
            col: 4,
            at: at + Duration::from_millis(300),
            ..first.clone()
        };
        assert!(is_repeat_click(&first, &near));
        let late = ClickSpot {
            at: at + Duration::from_millis(400),
            ..first.clone()
        };
        assert!(!is_repeat_click(&first, &late));
        let other_pane = ClickSpot {
            pane_id: "p2".into(),
            ..near
        };
        assert!(!is_repeat_click(&first, &other_pane));
    }
}
