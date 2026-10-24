//! The GUI's side of oasis's embeddable `ClientCore`: one core thread owns every server
//! connection (local and saved SSH machines) and raises a single wake fd when there is
//! something to drain, so the GTK loop never polls.

use std::cell::RefCell;
use std::io;
use std::os::fd::RawFd;

use oasis_core::embed::sidebar::SidebarPrefs;
use oasis_core::embed::wire::{ClientShellSnapshot, Method};
use oasis_core::embed::{ClientCore, CoreOptions, Drained, EndpointId, MachineSource, PaneInput};

pub struct Geometry {
    pub cols: u16,
    pub rows: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
}

pub struct Conn {
    core: ClientCore,
    active: RefCell<EndpointId>,
}

impl Conn {
    pub fn start(geometry: &Geometry, sidebar: SidebarPrefs) -> io::Result<Self> {
        let core = ClientCore::start(CoreOptions {
            surface_size: (geometry.cols, geometry.rows),
            cell_px: (geometry.cell_width_px, geometry.cell_height_px),
            mouse_capture: true,
            endpoint_keybindings: false,
            machines: MachineSource::SavedCatalog,
            local: true,
            server_exe: server_exe(),
            sidebar: Some(sidebar),
        })?;
        Ok(Self {
            core,
            active: RefCell::new(EndpointId::local()),
        })
    }

    pub fn wake_fd(&self) -> RawFd {
        self.core.wake_fd()
    }

    pub fn drain(&self) -> Drained {
        self.core.drain()
    }

    pub fn active(&self) -> EndpointId {
        self.active.borrow().clone()
    }

    pub fn set_active(&self, endpoint: &EndpointId) {
        self.active.replace(endpoint.clone());
        self.core.set_active(endpoint);
    }

    pub fn retry(&self, endpoint: &EndpointId) {
        self.core.retry(endpoint);
    }

    /// Sends a typed request, as the key engine produces, to the active machine.
    pub fn request_method(&self, method: Method) {
        self.core.request(&self.active(), method);
    }

    pub fn set_sidebar_prefs(&self, prefs: SidebarPrefs) {
        self.core.set_sidebar_prefs(prefs);
    }

    /// Sends one API operation to the active machine, e.g. `("tab.create", {"focus": true})`.
    pub fn request(&self, method: &str, params: serde_json::Value) -> Result<(), String> {
        let method: Method =
            serde_json::from_value(serde_json::json!({ "method": method, "params": params }))
                .map_err(|error| format!("{method}: {error}"))?;
        self.core.request(&self.active(), method);
        Ok(())
    }

    pub fn pane_input(&self, pane_id: &str, events: Vec<PaneInput>) {
        if !events.is_empty() {
            self.core.pane_input(&self.active(), pane_id, events);
        }
    }

    pub fn resize(&self, geometry: &Geometry) {
        self.core
            .set_cell_px(geometry.cell_width_px, geometry.cell_height_px);
        self.core.resize_surface(geometry.cols, geometry.rows);
    }

    pub fn focus(&self, focused: bool) {
        self.core.set_focus(focused);
    }
}

/// Prefer the `oasis` installed beside this binary: launchers may not put ~/.local/bin
/// on PATH. `None` lets the core run `oasis` from PATH.
fn server_exe() -> Option<std::path::PathBuf> {
    if let Some(exe) = std::env::var_os("CLAWMUX_OASIS") {
        return Some(exe.into());
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("oasis")))
        .filter(|path| path.is_file())
}

/// One machine the core knows about, as the sidebar shows it.
pub struct Machine {
    pub id: EndpointId,
    pub label: String,
    pub status: oasis_core::embed::EndpointStatus,
    pub snapshot: Option<Box<ClientShellSnapshot>>,
}
