use super::*;
use crate::api::schema::{PaneLinkActivateParams, PaneLinkRegion};
use crate::protocol::SurfaceRect;
use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LinkHoverTarget {
    pane_id: String,
    inner_rect: Rect,
    source_rect: SurfaceRect,
    content_revision: u64,
    offset_from_bottom: Option<u64>,
    row: u16,
    col: u16,
}

impl LinkHoverTarget {
    /// Same pane geometry and scroll position, whatever the content revision.
    fn same_place(&self, other: &Self) -> bool {
        self.pane_id == other.pane_id
            && self.inner_rect == other.inner_rect
            && self.source_rect == other.source_rect
            && self.offset_from_bottom == other.offset_from_bottom
    }
}

pub(super) struct LinkHover {
    target: LinkHoverTarget,
    pub(super) regions: Vec<PaneLinkRegion>,
    resolved: bool,
}

impl ClientShellState {
    pub(super) fn clear_link_hover(&mut self) -> bool {
        self.link_hover
            .take()
            .is_some_and(|hover| !hover.regions.is_empty())
    }

    fn link_hover_allowed(&self) -> bool {
        self.config.mouse_capture
            && self.mode == ClientShellMode::Terminal
            && self.overlay.is_none()
            && self.popup_terminal_id.is_none()
            && self.outer_focused != Some(false)
            && self.chrome_drag.is_none()
            && self.pane_mouse_gesture.is_none()
    }

    fn link_hover_target_current(&self, target: &LinkHoverTarget) -> bool {
        self.link_hover_allowed()
            && self.pane_surface.as_ref().is_some_and(|surface| {
                self.snapshot.as_ref().is_some_and(|snapshot| {
                    surface.boot_id == snapshot.boot_id
                        && surface.projection_revision == snapshot.revision
                }) && surface.panes.iter().any(|pane| {
                    pane.pane_id == target.pane_id
                        && pane.inner_rect == target.source_rect
                        && pane.content_revision == target.content_revision
                        && pane.scroll.map(|scroll| scroll.offset_from_bottom)
                            == target.offset_from_bottom
                })
            })
            && self
                .hits
                .panes
                .iter()
                .any(|hit| hit.pane_id == target.pane_id && hit.inner_rect == target.inner_rect)
    }

    /// Keeps a hover across redraws of its pane. Agent UIs redraw constantly, so
    /// a new content revision keeps the underline shown and re-resolves it; only
    /// a moved, resized, scrolled or closed pane drops it.
    pub(super) fn invalidate_link_hover(&mut self) {
        let Some(hover) = self.link_hover.as_ref() else {
            return;
        };
        if self.link_hover_target_current(&hover.target) {
            return;
        }
        let revision = self.pane_surface.as_ref().and_then(|surface| {
            surface
                .panes
                .iter()
                .find(|pane| {
                    pane.pane_id == hover.target.pane_id
                        && pane.inner_rect == hover.target.source_rect
                        && pane.scroll.map(|scroll| scroll.offset_from_bottom)
                            == hover.target.offset_from_bottom
                })
                .map(|pane| pane.content_revision)
        });
        let retargeted = revision.map(|content_revision| LinkHoverTarget {
            content_revision,
            ..hover.target.clone()
        });
        match retargeted.filter(|target| self.link_hover_target_current(target)) {
            Some(target) => self.retarget_link_hover(target),
            None => {
                self.clear_link_hover();
            }
        }
    }

    /// Points the hover at a newer content revision of the same place, keeping
    /// its regions until a resolve says otherwise.
    fn retarget_link_hover(&mut self, target: LinkHoverTarget) {
        let explicit = self.explicit_link_regions(&target);
        let Some(hover) = self.link_hover.as_mut() else {
            return;
        };
        let pointer_moved = hover.target.row != target.row || hover.target.col != target.col;
        if !pointer_moved {
            if hover.target.content_revision == target.content_revision {
                return;
            }
        } else {
            hover.target.row = target.row;
            hover.target.col = target.col;
        }
        hover.target.content_revision = target.content_revision;
        match explicit {
            Some(regions) => {
                hover.regions = regions;
                hover.resolved = true;
            }
            // An odd revision is mid-update: resolve once the pane settles.
            None => hover.resolved = !target.content_revision.is_multiple_of(2),
        }
    }

    /// Whether a hover is waiting to be resolved and nothing is in flight.
    pub(super) fn link_hover_resolve_due(&self) -> bool {
        self.link_hover
            .as_ref()
            .is_some_and(|hover| !hover.resolved)
            && !self.link_hover_resolve_blocked()
    }

    /// A lookup is already out, or an action for the active server is
    /// waiting: hover lookups share its single command lane and must not
    /// delay the action. The lookup is issued once the action is answered.
    fn link_hover_resolve_blocked(&self) -> bool {
        let active_boot = self
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.boot_id.as_str());
        self.pending_requests.values().any(|pending| {
            matches!(pending.kind, PendingEndpointKind::PaneLinkResolve { .. })
                || Some(pending.boot_id.as_str()) == active_boot
        })
    }

    /// Issues the resolve a redraw left pending. One-shot: a request is sent,
    /// or the hover is settled or dropped, so this never re-arms itself.
    pub(crate) fn tick_link_hover(&mut self, outcome: &mut ClientShellInput) {
        if !self.link_hover_resolve_due() {
            return;
        }
        if self
            .link_hover
            .as_ref()
            .is_some_and(|hover| self.link_hover_target_current(&hover.target))
        {
            self.request_link_hover(outcome);
            if let Some(hover) = self.link_hover.as_mut() {
                if !hover.resolved
                    && !self.pending_requests.values().any(|pending| {
                        matches!(pending.kind, PendingEndpointKind::PaneLinkResolve { .. })
                    })
                {
                    hover.resolved = true;
                }
            }
        } else {
            outcome.repaint |= self.clear_link_hover();
        }
    }

    /// Whether a plain (unmodified) hover may underline in this pane: the
    /// same rule as a plain click opening it.
    fn plain_hover_allowed(&self, pane_id: &str) -> bool {
        self.plain_click_may_open(pane_id)
    }

    pub(super) fn update_link_hover(&mut self, mouse: MouseEvent, outcome: &mut ClientShellInput) {
        if mouse.kind != MouseEventKind::Moved || !self.link_hover_allowed() {
            outcome.repaint |= self.clear_link_hover();
            return;
        }
        let Some(hit) = self
            .hits
            .panes
            .iter()
            .find(|hit| contains(hit.inner_rect, (mouse.column, mouse.row)))
        else {
            outcome.repaint |= self.clear_link_hover();
            return;
        };
        let wanted = mouse.modifiers.contains(KeyModifiers::CONTROL)
            || (mouse.modifiers.is_empty() && self.plain_hover_allowed(&hit.pane_id));
        if !wanted {
            outcome.repaint |= self.clear_link_hover();
            return;
        }
        let Some(pane) = self.pane_surface.as_ref().and_then(|surface| {
            surface
                .panes
                .iter()
                .find(|pane| pane.pane_id == hit.pane_id)
        }) else {
            outcome.repaint |= self.clear_link_hover();
            return;
        };
        let target = LinkHoverTarget {
            pane_id: hit.pane_id.clone(),
            inner_rect: hit.inner_rect,
            source_rect: pane.inner_rect,
            content_revision: pane.content_revision,
            offset_from_bottom: pane.scroll.map(|scroll| scroll.offset_from_bottom),
            row: mouse.row.saturating_sub(hit.inner_rect.y),
            col: mouse.column.saturating_sub(hit.inner_rect.x),
        };
        if !self.link_hover_target_current(&target) {
            outcome.repaint |= self.clear_link_hover();
            return;
        }
        if self.link_hover.as_ref().is_some_and(|hover| {
            hover.target.same_place(&target)
                && ((hover.target.row == target.row && hover.target.col == target.col)
                    || hover
                        .regions
                        .iter()
                        .any(|region| region_contains(region, target.col, target.row)))
        }) {
            // Still over the same link: keep the underline, follow redraws.
            let mut keep = target;
            if let Some(hover) = self.link_hover.as_ref() {
                keep.row = hover.target.row;
                keep.col = hover.target.col;
            }
            self.retarget_link_hover(keep);
            self.request_link_hover(outcome);
            return;
        }
        if !target.content_revision.is_multiple_of(2) {
            outcome.repaint |= self.clear_link_hover();
            return;
        }
        outcome.repaint |= self.clear_link_hover();
        let explicit = self.explicit_link_regions(&target);
        let resolved = explicit.is_some();
        let regions = explicit.unwrap_or_default();
        outcome.repaint |= !regions.is_empty();
        self.link_hover = Some(LinkHover {
            target,
            regions,
            resolved,
        });
        self.request_link_hover(outcome);
    }

    fn request_link_hover(&mut self, outcome: &mut ClientShellInput) {
        if self.link_hover_resolve_blocked() {
            return;
        }
        let Some(hover) = self.link_hover.as_ref().filter(|hover| !hover.resolved) else {
            return;
        };
        if !self.link_hover_target_current(&hover.target) {
            return;
        }
        let target = hover.target.clone();
        // Hover is optional and speculative: do not probe unadvertised methods or show notices.
        if !self.endpoint_is_online(&self.active_endpoint_id)
            || !self
                .endpoints
                .iter()
                .find(|endpoint| endpoint.endpoint_id == self.active_endpoint_id)
                .and_then(|endpoint| endpoint.methods.as_ref())
                .is_some_and(|methods| methods.iter().any(|method| method == "pane.link.resolve"))
        {
            if let Some(hover) = self.link_hover.as_mut() {
                hover.resolved = true;
            }
            return;
        }
        self.push_endpoint_method_with_kind(
            crate::api::schema::Method::PaneLinkResolve(PaneLinkActivateParams {
                pane_id: target.pane_id.clone(),
                viewport_row: target.row,
                col: target.col,
                content_revision: Some(target.content_revision),
                offset_from_bottom: target.offset_from_bottom,
            }),
            PendingEndpointKind::PaneLinkResolve { target },
            outcome,
        );
    }

    pub(super) fn complete_link_hover(
        &mut self,
        target: LinkHoverTarget,
        result: Result<crate::api::schema::ResponseResult, ClientShellEndpointError>,
    ) -> (bool, Vec<ClientShellAction>) {
        let cancelled = result
            .as_ref()
            .err()
            .is_some_and(|error| error.code.as_deref() == Some("endpoint_cancelled"));
        let stale = result
            .as_ref()
            .err()
            .is_some_and(|error| error.code.as_deref() == Some("stale_content"));
        let mut outcome = ClientShellInput::default();
        if stale {
            // The pane redrew while the request was in flight. Keep what is
            // shown; the next surface for the new revision re-resolves it.
            if let Some(hover) = self
                .link_hover
                .as_mut()
                .filter(|hover| hover.target == target)
            {
                hover.resolved = true;
            }
            self.request_link_hover(&mut outcome);
            return (outcome.repaint, outcome.actions);
        }
        if self.link_hover_target_current(&target)
            && self
                .link_hover
                .as_ref()
                .is_some_and(|hover| hover.target == target)
        {
            let regions = match result {
                Ok(crate::api::schema::ResponseResult::PaneLinkResolved { regions })
                    if regions.len() <= usize::from(target.inner_rect.height)
                        && regions.iter().all(|region| {
                            region.row < target.inner_rect.height
                                && region.start_col <= region.end_col
                                && region.end_col < target.inner_rect.width
                        })
                        && regions
                            .iter()
                            .any(|region| region_contains(region, target.col, target.row)) =>
                {
                    regions
                }
                _ => Vec::new(),
            };
            outcome.repaint = !regions.is_empty();
            self.link_hover = Some(LinkHover {
                target,
                regions,
                resolved: true,
            });
        }
        if !cancelled {
            self.request_link_hover(&mut outcome);
        }
        (outcome.repaint, outcome.actions)
    }

    fn explicit_link_regions(&self, target: &LinkHoverTarget) -> Option<Vec<PaneLinkRegion>> {
        let surface = self.pane_surface.as_ref()?;
        let rect = target.source_rect;
        let cell = |col: u16, row: u16| {
            let x = usize::from(rect.x) + usize::from(col);
            let y = usize::from(rect.y) + usize::from(row);
            if x >= usize::from(surface.frame.width) || y >= usize::from(surface.frame.height) {
                return None;
            }
            surface
                .frame
                .cells
                .get(y * usize::from(surface.frame.width) + x)
        };
        let hyperlink_at = |col: u16, row: u16| {
            let current = cell(col, row)?;
            current.hyperlink.or_else(|| {
                // Rendered wide-cell spacers may not carry their own OSC 8 metadata.
                if col == 0 || !current.symbol.trim().is_empty() {
                    return None;
                }
                let previous = cell(col - 1, row)?;
                (previous.symbol.width() == 2)
                    .then_some(previous.hyperlink)
                    .flatten()
            })
        };
        let hyperlink = hyperlink_at(target.col, target.row)?;
        let uri = surface.frame.hyperlinks.get(hyperlink as usize)?;
        if crate::app::actions::safe_web_url(uri).is_none() {
            return Some(Vec::new());
        }
        let width = usize::from(rect.width);
        if width == 0 {
            return Some(Vec::new());
        }
        let clicked = usize::from(target.row) * width + usize::from(target.col);
        let mut start = clicked;
        let mut end = clicked;
        let matches = |index: usize| {
            hyperlink_at((index % width) as u16, (index / width) as u16) == Some(hyperlink)
        };
        // Frame IDs identify destinations, not OSC 8 runs. Adjacent cells with
        // the same destination form one hover region, including across rows.
        let mut remaining = 8191usize;
        while start > 0 && matches(start - 1) {
            if remaining == 0 {
                return Some(Vec::new());
            }
            remaining -= 1;
            start -= 1;
        }
        while end + 1 < width * usize::from(rect.height) && matches(end + 1) {
            if remaining == 0 {
                return Some(Vec::new());
            }
            remaining -= 1;
            end += 1;
        }
        let regions = (start / width..=end / width)
            .map(|row| PaneLinkRegion {
                row: row as u16,
                start_col: if row == start / width {
                    (start % width) as u16
                } else {
                    0
                },
                end_col: if row == end / width {
                    (end % width) as u16
                } else {
                    rect.width - 1
                },
            })
            .collect();
        Some(regions)
    }

    pub(super) fn link_hover_blocks_patch(
        &self,
        patch: &crate::protocol::PaneSurfacePatch,
    ) -> bool {
        self.link_hover.as_ref().is_some_and(|hover| {
            !hover.regions.is_empty()
                && patch
                    .panes
                    .iter()
                    .any(|pane| pane.pane_id == hover.target.pane_id)
        })
    }

    pub(super) fn render_link_hover(
        &self,
        frame: &mut FrameData,
        occlusion: &mut crate::kitty_graphics::surface::Occlusion,
    ) {
        let Some(hover) = self
            .link_hover
            .as_ref()
            .filter(|hover| self.link_hover_target_current(&hover.target))
        else {
            return;
        };
        let rect = hover.target.inner_rect;
        for region in &hover.regions {
            let row = rect.y.saturating_add(region.row);
            if row >= frame.height {
                continue;
            }
            occlusion.cover(
                Rect::new(
                    rect.x.saturating_add(region.start_col),
                    row,
                    region
                        .end_col
                        .saturating_sub(region.start_col)
                        .saturating_add(1),
                    1,
                )
                .intersection(Rect::new(0, 0, frame.width, frame.height)),
            );
            for col in region.start_col..=region.end_col {
                let col = rect.x.saturating_add(col);
                if col >= frame.width {
                    continue;
                }
                if let Some(cell) = frame
                    .cells
                    .get_mut(usize::from(row) * usize::from(frame.width) + usize::from(col))
                {
                    cell.modifier |= Modifier::UNDERLINED.bits();
                }
            }
        }
    }
}

fn region_contains(region: &PaneLinkRegion, col: u16, row: u16) -> bool {
    region.row == row && col >= region.start_col && col <= region.end_col
}
