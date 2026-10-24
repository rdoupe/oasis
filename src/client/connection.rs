//! Client-shell connection pieces shared by the TUI loop and the embeddable
//! [`crate::embed::ClientCore`]: the per-connection surface decoder, the
//! endpoint reader thread, and applying surface patches to a committed surface.

use super::*;
use crate::protocol::FrameData;

/// The decoder for whichever optional surface encodings this connection negotiated.
///
/// It expands surface reuse, deltas and scroll-aware patches back into plain
/// `PaneSurface` and `PaneSurfacePatch` messages before they reach a client.
pub(crate) fn surface_decoder(
    negotiation: &endpoint::EndpointNegotiation,
) -> Option<protocol::surface_reuse::Decoder> {
    let reuse = negotiation.supports_capability(protocol::surface_reuse::CAPABILITY);
    let delta = negotiation.supports_capability(protocol::surface_delta::CAPABILITY);
    let scroll = negotiation.supports_capability(protocol::surface_scroll::CAPABILITY);
    (reuse || delta || scroll).then(|| protocol::surface_reuse::Decoder::new(delta, scroll))
}

/// Starts the reader for a connected endpoint. It forwards decoded messages
/// as [`ClientLoopEvent::ServerMessage`]; the writer's stop handle shutting
/// the socket down ends it with [`ClientLoopEvent::ServerDisconnected`].
pub(super) fn spawn_endpoint_reader(
    reader: LocalStream,
    stop: Arc<AtomicBool>,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    endpoint_id: endpoint::ClientEndpointId,
    generation: u64,
    negotiation: &endpoint::EndpointNegotiation,
) {
    let surface_decoder = surface_decoder(negotiation);
    if let Err(error) = transport::spawn_server_reader(
        reader,
        stop,
        event_tx.clone(),
        MAX_GRAPHICS_FRAME_SIZE,
        endpoint_id.clone(),
        generation,
        surface_decoder,
    ) {
        warn!(%error, "endpoint reader could not start");
        let _ = event_tx.try_send(ClientLoopEvent::ServerDisconnected {
            endpoint_id,
            generation,
        });
    }
}

pub(crate) fn row_fits_frame(row: &protocol::PaneSurfacePatchRow, frame: &FrameData) -> bool {
    row.x
        .saturating_add(row.cells.len().min(u16::MAX as usize) as u16)
        <= frame.width
        && row.y < frame.height
}

pub(crate) fn apply_row(row: &protocol::PaneSurfacePatchRow, frame: &mut FrameData) -> bool {
    if !row_fits_frame(row, frame) {
        return false;
    }
    let start = usize::from(row.y) * usize::from(frame.width) + usize::from(row.x);
    let end = start + row.cells.len();
    if end > frame.cells.len() {
        return false;
    }
    frame.cells[start..end].clone_from_slice(&row.cells);
    true
}

/// Applies `patch` to the committed `surface`. Returns false, leaving the
/// surface partly updated, when the patch does not fit it.
pub(crate) fn apply_patch_to_surface(
    surface: &mut protocol::PaneSurfaceFrame,
    patch: &protocol::PaneSurfacePatch,
) -> bool {
    for row in &patch.rows {
        if !apply_row(row, &mut surface.frame) {
            return false;
        }
    }
    for updated in &patch.panes {
        let Some(existing) = surface
            .panes
            .iter_mut()
            .find(|pane| pane.pane_id == updated.pane_id)
        else {
            return false;
        };
        *existing = updated.clone();
    }
    surface.frame.cursor = patch.cursor.clone();
    surface.surface_revision = patch.surface_revision;
    true
}
