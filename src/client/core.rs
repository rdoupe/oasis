//! The embeddable client core: client-shell connections to Local and saved
//! machines on a thread of its own, delivered to a front end through one wake
//! signal and a coalescing outbox. Front ends reach it as [`crate::embed`].
//!
//! The core does no work while nothing changes: it sleeps until a server
//! message, a command, a scheduled reconnect, or (only while a remote machine
//! is connected) the next connection health check.

use super::endpoint::{
    ClientEndpointId, ClientEndpointStatus, EndpointConnectOptions, EndpointRegistry,
    EndpointSupervisorEvent, EndpointSupervisors, SavedSshEndpoint,
};
use super::events::ClientLoopEvent;
use crate::protocol::{
    ClientHostThemeUpdate, ClientMessage, ClientPaneInputEvent, ClientShellSnapshot,
    ClientSurfaceSize, PaneSurfaceFrame, PaneSurfacePane, PaneSurfacePatch, PaneSurfacePatchRow,
    SemanticNotification, ServerMessage,
};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Discrete events beyond this many are dropped oldest-first while a front end
/// is not draining, so a hidden window cannot grow the outbox without bound.
const MAX_QUEUED_EVENTS: usize = 1024;
/// How often connection health is checked while a remote machine is connected.
/// Local connections have no health check, so a Local-only core never ticks.
const HEALTH_TICK: Duration = Duration::from_millis(2500);
/// How long the core waits for a Local server it started to open its socket.
const LOCAL_START_TIMEOUT: Duration = Duration::from_secs(5);

/// How the core connects. Built with [`CoreOptions::default`] and adjusted.
#[derive(Clone, Debug)]
pub struct CoreOptions {
    /// Pane-area size in cells the front end renders.
    pub surface_size: (u16, u16),
    /// Cell size in pixels, for pixel mouse input and graphics geometry.
    pub cell_px: (u32, u32),
    pub mouse_capture: bool,
    /// Ask servers for their own keybindings with each snapshot.
    pub endpoint_keybindings: bool,
    pub machines: MachineSource,
    /// Connect to the Local server, starting it when it is not running.
    pub local: bool,
    /// The executable that starts a Local server. `None` runs `oasis` from PATH.
    pub server_exe: Option<PathBuf>,
    /// Build a [`super::model::SidebarModel`] from snapshots and deliver it
    /// through [`Drained::sidebar`] when it changes. `None` skips the work.
    pub sidebar: Option<super::model::SidebarPrefs>,
}

impl Default for CoreOptions {
    fn default() -> Self {
        Self {
            surface_size: (80, 24),
            cell_px: (0, 0),
            mouse_capture: true,
            endpoint_keybindings: false,
            machines: MachineSource::SavedCatalog,
            local: true,
            server_exe: None,
            sidebar: None,
        }
    }
}

/// Which saved SSH machines the core connects to.
#[derive(Clone, Debug)]
pub enum MachineSource {
    /// The machines saved in the oasis catalog, as the TUI reads them at start.
    SavedCatalog,
    Explicit(Vec<SavedMachine>),
    None,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedMachine {
    pub label: String,
    /// The ssh destination, such as `user@host` or a config alias.
    pub target: String,
    /// The Herdr or Oasis session on that machine.
    pub session: String,
}

/// One connection target: Local or a saved machine.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EndpointId(String);

impl EndpointId {
    pub fn local() -> Self {
        Self::from_client(&ClientEndpointId::Local)
    }

    pub fn is_local(&self) -> bool {
        self.0 == "local"
    }

    /// The stable key the TUI also stores for this endpoint.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_client(id: &ClientEndpointId) -> Self {
        Self(id.storage_key())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointStatus {
    Connecting,
    Online,
    /// Lost and retrying; carries the last failure.
    Reconnecting(String),
    /// Failing in a way a person must fix, such as a version or generation
    /// mismatch, or ssh refusing the login.
    Attention(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndpointError {
    pub code: Option<String>,
    pub message: String,
}

/// What changed since the last [`ClientCore::drain`].
#[derive(Debug, Default)]
pub struct Drained {
    /// Endpoints with new status, snapshot or surface, in a stable order.
    pub endpoints: Vec<EndpointUpdate>,
    /// Discrete events in arrival order; never coalesced.
    pub events: Vec<CoreEvent>,
    /// Events dropped because the outbox was full.
    pub dropped_events: u64,
    /// The sidebar model, when sidebar prefs are set and it changed.
    pub sidebar: Option<super::model::SidebarModel>,
}

impl Drained {
    pub fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
            && self.events.is_empty()
            && self.dropped_events == 0
            && self.sidebar.is_none()
    }
}

#[derive(Debug)]
pub struct EndpointUpdate {
    pub id: EndpointId,
    pub label: String,
    /// Set when the status changed.
    pub status: Option<EndpointStatus>,
    /// Negotiated methods and capabilities, set when the endpoint (re)connected.
    pub negotiation: Option<Negotiation>,
    /// The latest snapshot, when a new one arrived.
    pub snapshot: Option<Box<ClientShellSnapshot>>,
    /// Surface changes for the active endpoint.
    pub surface: Option<SurfaceUpdate>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Negotiation {
    pub methods: Vec<String>,
    pub capabilities: Vec<String>,
}

/// Surface changes since the last drain, coalesced.
#[derive(Debug)]
pub enum SurfaceUpdate {
    /// Replace the whole surface: a new surface arrived (resize, reconnect,
    /// topology change), with any later patches already applied.
    Full(Box<PaneSurfaceFrame>),
    /// Rewrite these rows. Every changed row since the last drain appears once,
    /// full width, in row order, with the latest pane metadata and cursor.
    Patch {
        surface_revision: u64,
        rows: Vec<PaneSurfacePatchRow>,
        panes: Vec<PaneSurfacePane>,
        cursor: Option<crate::protocol::CursorState>,
    },
}

#[derive(Debug)]
pub enum CoreEvent {
    Response {
        id: RequestId,
        endpoint: EndpointId,
        result: Result<Box<crate::api::schema::ResponseResult>, EndpointError>,
    },
    Notification {
        endpoint: EndpointId,
        notification: SemanticNotification,
    },
    /// An immediate endpoint error the front end should show.
    Error {
        endpoint: EndpointId,
        message: String,
    },
    /// Whether the focused pane needs every key reported.
    KeyboardReportAll {
        endpoint: EndpointId,
        enabled: bool,
    },
    Bell {
        endpoint: EndpointId,
        count: u16,
    },
    /// `None` restores the default window title.
    Title {
        endpoint: EndpointId,
        title: Option<String>,
    },
    /// Decoded clipboard bytes a pane asked to copy.
    Clipboard {
        endpoint: EndpointId,
        data: Vec<u8>,
    },
}

enum Command {
    Request {
        id: RequestId,
        endpoint: EndpointId,
        method: Box<crate::api::schema::Method>,
    },
    PaneInput {
        endpoint: EndpointId,
        pane_id: String,
        events: Vec<ClientPaneInputEvent>,
    },
    SetActive(EndpointId),
    ResizeSurface(u16, u16),
    CellPx(u32, u32),
    Focus(bool),
    HostTheme(ClientHostThemeUpdate),
    Retry(EndpointId),
    SidebarPrefs(Box<super::model::SidebarPrefs>),
    Shutdown,
}

/// Client-shell connections for a native front end. See the module docs.
pub struct ClientCore {
    commands: tokio::sync::mpsc::UnboundedSender<Command>,
    shared: Arc<Shared>,
    next_request: std::sync::atomic::AtomicU64,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ClientCore {
    /// Starts the core thread and begins connecting.
    pub fn start(options: CoreOptions) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            outbox: Mutex::new(Outbox::default()),
            wake: crate::platform::CoreWake::new()?,
        });
        let (commands, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("oasis-core".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        tracing::warn!(%error, "client core runtime failed to start");
                        return;
                    }
                };
                runtime.block_on(CoreLoop::new(options, thread_shared, command_rx).run());
            })?;
        Ok(Self {
            commands,
            shared,
            next_request: std::sync::atomic::AtomicU64::new(1),
            thread: Some(thread),
        })
    }

    /// Readable while undrained changes wait; [`Self::drain`] clears it.
    #[cfg(unix)]
    pub fn wake_fd(&self) -> std::os::fd::RawFd {
        self.shared.wake.raw_fd()
    }

    /// Signalled while undrained changes wait; [`Self::drain`] resets it.
    #[cfg(windows)]
    pub fn wake_handle(&self) -> std::os::windows::io::RawHandle {
        self.shared.wake.raw_handle()
    }

    /// Takes everything that changed since the last drain. Never blocks.
    pub fn drain(&self) -> Drained {
        let mut outbox = self
            .shared
            .outbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.shared.wake.clear();
        outbox.take()
    }

    /// Sends one JSON endpoint request; its result arrives as
    /// [`CoreEvent::Response`] with the returned id.
    pub fn request(&self, endpoint: &EndpointId, method: crate::api::schema::Method) -> RequestId {
        let id = RequestId(
            self.next_request
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        );
        self.send(Command::Request {
            id,
            endpoint: endpoint.clone(),
            method: Box::new(method),
        });
        id
    }

    pub fn pane_input(
        &self,
        endpoint: &EndpointId,
        pane_id: &str,
        events: Vec<ClientPaneInputEvent>,
    ) {
        self.send(Command::PaneInput {
            endpoint: endpoint.clone(),
            pane_id: pane_id.to_owned(),
            events,
        });
    }

    /// Streams this endpoint's surface; the previous one stops streaming.
    pub fn set_active(&self, endpoint: &EndpointId) {
        self.send(Command::SetActive(endpoint.clone()));
    }

    pub fn resize_surface(&self, cols: u16, rows: u16) {
        self.send(Command::ResizeSurface(cols, rows));
    }

    pub fn set_cell_px(&self, width: u32, height: u32) {
        self.send(Command::CellPx(width, height));
    }

    /// Whether the front end's window has focus; forwarded to every server.
    pub fn set_focus(&self, focused: bool) {
        self.send(Command::Focus(focused));
    }

    /// Host colors or light/dark appearance, forwarded to every server.
    pub fn set_host_theme(&self, update: ClientHostThemeUpdate) {
        self.send(Command::HostTheme(update));
    }

    /// Retries a machine now instead of at its next scheduled attempt.
    pub fn retry(&self, endpoint: &EndpointId) {
        self.send(Command::Retry(endpoint.clone()));
    }

    /// Replaces the sidebar settings, such as collapsed groups or sort
    /// order, and turns the sidebar model on when it was off.
    pub fn set_sidebar_prefs(&self, prefs: super::model::SidebarPrefs) {
        self.send(Command::SidebarPrefs(Box::new(prefs)));
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

impl Drop for ClientCore {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Shared {
    outbox: Mutex<Outbox>,
    wake: crate::platform::CoreWake,
}

impl Shared {
    /// Updates the outbox and raises the wake only on the empty → non-empty
    /// transition, so a front end that drains late is woken once.
    fn publish(&self, update: impl FnOnce(&mut Outbox)) {
        let mut outbox = self
            .outbox
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let was_empty = outbox.is_empty();
        update(&mut outbox);
        if was_empty && !outbox.is_empty() {
            self.wake.raise();
        }
    }
}

#[derive(Default)]
struct Outbox {
    endpoints: BTreeMap<EndpointId, PendingEndpoint>,
    events: VecDeque<CoreEvent>,
    dropped_events: u64,
    sidebar: Option<super::model::SidebarModel>,
    /// Committed surfaces per endpoint, kept so coalesced patches can be
    /// expanded into full rows at drain time.
    surfaces: HashMap<EndpointId, PaneSurfaceFrame>,
}

#[derive(Default)]
struct PendingEndpoint {
    label: String,
    status: Option<EndpointStatus>,
    negotiation: Option<Negotiation>,
    snapshot: Option<Box<ClientShellSnapshot>>,
    surface: Option<PendingSurface>,
}

enum PendingSurface {
    Full,
    Rows {
        rows: BTreeSet<u16>,
        panes: BTreeMap<String, PaneSurfacePane>,
    },
}

impl Outbox {
    fn is_empty(&self) -> bool {
        self.endpoints.is_empty()
            && self.events.is_empty()
            && self.dropped_events == 0
            && self.sidebar.is_none()
    }

    fn endpoint(&mut self, id: &EndpointId, label: &str) -> &mut PendingEndpoint {
        let pending = self.endpoints.entry(id.clone()).or_default();
        label.clone_into(&mut pending.label);
        pending
    }

    fn event(&mut self, event: CoreEvent) {
        if self.events.len() >= MAX_QUEUED_EVENTS {
            self.events.pop_front();
            self.dropped_events += 1;
        }
        self.events.push_back(event);
    }

    fn surface_full(&mut self, id: &EndpointId, label: &str, surface: PaneSurfaceFrame) {
        self.surfaces.insert(id.clone(), surface);
        self.endpoint(id, label).surface = Some(PendingSurface::Full);
    }

    /// Applies a patch to the committed surface and records its rows. A patch
    /// that does not apply is dropped, as the TUI does; the server follows
    /// with a full surface.
    fn surface_patch(&mut self, id: &EndpointId, label: &str, patch: &PaneSurfacePatch) -> bool {
        let Some(surface) = self.surfaces.get_mut(id) else {
            return false;
        };
        if surface.boot_id != patch.boot_id
            || surface.surface_revision != patch.base_surface_revision
        {
            return false;
        }
        let mut next = surface.clone();
        if !super::connection::apply_patch_to_surface(&mut next, patch) {
            return false;
        }
        *surface = next;
        let pending = self.endpoint(id, label);
        match &mut pending.surface {
            Some(PendingSurface::Full) => {}
            Some(PendingSurface::Rows { rows, panes }) => {
                rows.extend(patch.rows.iter().map(|row| row.y));
                for pane in &patch.panes {
                    panes.insert(pane.pane_id.clone(), pane.clone());
                }
            }
            None => {
                pending.surface = Some(PendingSurface::Rows {
                    rows: patch.rows.iter().map(|row| row.y).collect(),
                    panes: patch
                        .panes
                        .iter()
                        .map(|pane| (pane.pane_id.clone(), pane.clone()))
                        .collect(),
                });
            }
        }
        true
    }

    fn take(&mut self) -> Drained {
        let pending = std::mem::take(&mut self.endpoints);
        let endpoints = pending
            .into_iter()
            .map(|(id, pending)| {
                let surface = pending.surface.and_then(|surface| {
                    let committed = self.surfaces.get(&id)?;
                    Some(match surface {
                        PendingSurface::Full => SurfaceUpdate::Full(Box::new(committed.clone())),
                        PendingSurface::Rows { rows, panes } => SurfaceUpdate::Patch {
                            surface_revision: committed.surface_revision,
                            rows: rows
                                .into_iter()
                                .filter_map(|y| full_row(committed, y))
                                .collect(),
                            panes: panes.into_values().collect(),
                            cursor: committed.frame.cursor.clone(),
                        },
                    })
                });
                EndpointUpdate {
                    id,
                    label: pending.label,
                    status: pending.status,
                    negotiation: pending.negotiation,
                    snapshot: pending.snapshot,
                    surface,
                }
            })
            .collect();
        Drained {
            endpoints,
            events: std::mem::take(&mut self.events).into(),
            dropped_events: std::mem::take(&mut self.dropped_events),
            sidebar: self.sidebar.take(),
        }
    }
}

fn full_row(surface: &PaneSurfaceFrame, y: u16) -> Option<PaneSurfacePatchRow> {
    let width = usize::from(surface.frame.width);
    let start = usize::from(y) * width;
    let cells = surface.frame.cells.get(start..start + width)?.to_vec();
    Some(PaneSurfacePatchRow { x: 0, y, cells })
}

/// State owned by the core thread.
struct CoreLoop {
    options: CoreOptions,
    shared: Arc<Shared>,
    commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
    registry: EndpointRegistry,
    supervisors: EndpointSupervisors,
    supervisor_tx: tokio::sync::mpsc::Sender<EndpointSupervisorEvent>,
    supervisor_rx: tokio::sync::mpsc::Receiver<EndpointSupervisorEvent>,
    reader_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    reader_rx: tokio::sync::mpsc::Receiver<ClientLoopEvent>,
    labels: HashMap<ClientEndpointId, String>,
    /// Latest snapshot boot id per endpoint, needed to address requests.
    boot_ids: HashMap<ClientEndpointId, String>,
    active: ClientEndpointId,
    /// Endpoints currently asked to stream their surface.
    streaming: BTreeSet<String>,
    responses: HashMap<(ClientEndpointId, String), (RequestId, Vec<u8>)>,
    next_internal_request: u64,
    focused: bool,
    next_health_tick: Option<Instant>,
    /// Endpoint order for the sidebar: Local first, then saved machines.
    order: Vec<ClientEndpointId>,
    sidebar: Option<CoreSidebar>,
}

/// The sidebar model's inputs, kept on the core thread.
struct CoreSidebar {
    prefs: super::model::SidebarPrefs,
    model: super::model::SidebarModel,
    snapshots: HashMap<ClientEndpointId, Box<ClientShellSnapshot>>,
    online: std::collections::HashSet<ClientEndpointId>,
}

impl CoreSidebar {
    fn new(prefs: super::model::SidebarPrefs) -> Self {
        Self {
            prefs,
            model: super::model::SidebarModel::default(),
            snapshots: HashMap::new(),
            online: std::collections::HashSet::new(),
        }
    }
}

/// Rebuilds `sidebar` from its endpoints in `order` and publishes the model
/// only when something a sidebar shows changed.
fn publish_sidebar(
    shared: &Shared,
    sidebar: &mut CoreSidebar,
    order: &[ClientEndpointId],
    labels: &HashMap<ClientEndpointId, String>,
) {
    let ids = order
        .iter()
        .map(|id| (id, id.storage_key()))
        .collect::<Vec<_>>();
    let endpoints = ids
        .iter()
        .map(|(id, key)| super::model::SidebarEndpoint {
            id: key,
            label: labels.get(*id).map_or(key.as_str(), String::as_str),
            local: id.is_local(),
            online: sidebar.online.contains(*id),
            snapshot: sidebar.snapshots.get(*id).map(Box::as_ref),
        })
        .collect::<Vec<_>>();
    if sidebar.model.update(&endpoints, &sidebar.prefs) {
        let model = sidebar.model.clone();
        shared.publish(|outbox| outbox.sidebar = Some(model));
    }
}

impl CoreLoop {
    fn new(
        options: CoreOptions,
        shared: Arc<Shared>,
        commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
    ) -> Self {
        let now = Instant::now();
        let profiles: Vec<SavedSshEndpoint> = match &options.machines {
            MachineSource::SavedCatalog => super::endpoint::EndpointCatalog::load()
                .map(|catalog| catalog.ssh)
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "saved machines could not be loaded");
                    Vec::new()
                }),
            MachineSource::Explicit(machines) => machines
                .iter()
                .filter_map(|machine| {
                    SavedSshEndpoint::new(&machine.label, &machine.target, &machine.session)
                        .map_err(|error| tracing::warn!(%error, "invalid saved machine"))
                        .ok()
                })
                .collect(),
            MachineSource::None => Vec::new(),
        };
        let mut labels: HashMap<ClientEndpointId, String> = profiles
            .iter()
            .filter(|profile| profile.enabled)
            .map(|profile| {
                (
                    ClientEndpointId::Ssh(profile.id.clone()),
                    profile.label.clone(),
                )
            })
            .collect();
        let mut supervisors = EndpointSupervisors::new(&profiles, now);
        let first_machine = labels.keys().next().cloned();
        let mut order = Vec::new();
        if options.local {
            order.push(ClientEndpointId::Local);
        }
        order.extend(
            profiles
                .iter()
                .filter(|profile| profile.enabled)
                .map(|profile| ClientEndpointId::Ssh(profile.id.clone())),
        );
        let sidebar = options.sidebar.clone().map(CoreSidebar::new);
        if options.local {
            labels.insert(ClientEndpointId::Local, "Local".into());
            supervisors.add_local(super::client_socket_path(), None, now);
        }
        let (supervisor_tx, supervisor_rx) = tokio::sync::mpsc::channel(64);
        let (reader_tx, reader_rx) = tokio::sync::mpsc::channel(256);
        let active = if options.local {
            ClientEndpointId::Local
        } else {
            first_machine.unwrap_or(ClientEndpointId::Local)
        };
        Self {
            options,
            shared,
            commands,
            registry: EndpointRegistry::empty(),
            supervisors,
            supervisor_tx,
            supervisor_rx,
            reader_tx,
            reader_rx,
            labels,
            boot_ids: HashMap::new(),
            active,
            streaming: BTreeSet::new(),
            responses: HashMap::new(),
            next_internal_request: 1,
            focused: true,
            next_health_tick: None,
            order,
            sidebar,
        }
    }

    /// Rebuilds the sidebar model and publishes it when it changed.
    fn refresh_sidebar(&mut self) {
        if let Some(sidebar) = self.sidebar.as_mut() {
            publish_sidebar(&self.shared, sidebar, &self.order, &self.labels);
        }
    }

    fn sidebar_online(&mut self, id: &ClientEndpointId, online: bool) {
        let Some(sidebar) = self.sidebar.as_mut() else {
            return;
        };
        let changed = if online {
            sidebar.online.insert(id.clone())
        } else {
            sidebar.online.remove(id)
        };
        if changed {
            self.refresh_sidebar();
        }
    }

    async fn run(mut self) {
        if self.options.local {
            self.start_local_if_needed().await;
        }
        loop {
            let now = Instant::now();
            self.spawn_due(now);
            let deadline = [
                self.supervisors.next_attempt_due(now),
                self.next_health_tick,
            ]
            .into_iter()
            .flatten()
            .min();
            tokio::select! {
                command = self.commands.recv() => match command {
                    Some(Command::Shutdown) | None => break,
                    Some(command) => self.command(command),
                },
                Some(event) = self.supervisor_rx.recv() => self.supervisor_event(event),
                Some(event) = self.reader_rx.recv() => self.reader_event(event),
                _ = sleep_until(deadline) => {
                    if self.next_health_tick.is_some_and(|tick| tick <= Instant::now()) {
                        self.registry.tick_health(Instant::now());
                        self.schedule_health();
                    }
                }
            }
            self.handle_failures();
        }
        for id in self.labels.keys().cloned().collect::<Vec<_>>() {
            self.registry.disconnect(&id);
        }
    }

    async fn start_local_if_needed(&mut self) {
        let path = super::client_socket_path();
        if crate::ipc::connect_local_stream(&path).is_ok() {
            return;
        }
        let exe = self
            .options
            .server_exe
            .clone()
            .unwrap_or_else(|| PathBuf::from("oasis"));
        let started = tokio::task::spawn_blocking(move || {
            crate::server::autodetect::spawn_server_daemon_with(exe)?;
            crate::server::autodetect::wait_for_server_socket(&path, LOCAL_START_TIMEOUT)
        })
        .await;
        let error = match started {
            Ok(Ok(())) => return,
            Ok(Err(error)) => error.to_string(),
            Err(error) => error.to_string(),
        };
        let id = ClientEndpointId::Local;
        let label = self.label(&id);
        self.shared.publish(|outbox| {
            outbox
                .endpoint(&EndpointId::from_client(&id), &label)
                .status = Some(EndpointStatus::Attention(format!(
                "Local server did not start: {error}"
            )));
        });
    }

    fn connect_options(&self) -> EndpointConnectOptions {
        EndpointConnectOptions {
            cols: self.options.surface_size.0,
            rows: self.options.surface_size.1,
            cell_width_px: self.options.cell_px.0,
            cell_height_px: self.options.cell_px.1,
            pixel_geometry_exact: self.options.cell_px.0 > 0 && self.options.cell_px.1 > 0,
            pixel_mouse: self.options.cell_px.0 > 0 && self.options.cell_px.1 > 0,
            surface_size: self.surface_size(),
            endpoint_keybindings: self.options.endpoint_keybindings,
            mouse_capture: self.options.mouse_capture,
        }
    }

    fn surface_size(&self) -> ClientSurfaceSize {
        ClientSurfaceSize {
            cols: self.options.surface_size.0,
            rows: self.options.surface_size.1,
        }
    }

    fn spawn_due(&mut self, now: Instant) {
        let options = self.connect_options();
        self.supervisors
            .spawn_due(now, options, &self.supervisor_tx);
    }

    fn label(&self, id: &ClientEndpointId) -> String {
        self.labels
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.storage_key())
    }

    fn schedule_health(&mut self) {
        self.next_health_tick = self
            .registry
            .health_tracked()
            .then(|| Instant::now() + HEALTH_TICK);
    }

    fn supervisor_event(&mut self, event: EndpointSupervisorEvent) {
        let now = Instant::now();
        match event {
            EndpointSupervisorEvent::Status {
                endpoint_id,
                generation,
                status,
                message,
            } => {
                if !self
                    .supervisors
                    .record_status(&endpoint_id, generation, status, now)
                {
                    return;
                }
                self.publish_status(&endpoint_id, core_status(status, message));
            }
            EndpointSupervisorEvent::Connected {
                endpoint_id,
                generation,
                reader,
                writer,
                negotiation,
            } => {
                if !self.supervisors.record_status(
                    &endpoint_id,
                    generation,
                    ClientEndpointStatus::Online,
                    now,
                ) {
                    return;
                }
                let stop = writer.stop_handle();
                let published = Negotiation {
                    methods: negotiation.methods(),
                    capabilities: negotiation.capabilities(),
                };
                super::connection::spawn_endpoint_reader(
                    reader,
                    stop,
                    self.reader_tx.clone(),
                    endpoint_id.clone(),
                    generation,
                    &negotiation,
                );
                self.registry
                    .insert(endpoint_id.clone(), writer, generation, negotiation, false);
                self.streaming.remove(&endpoint_id.storage_key());
                let _ = self.registry.send_to(
                    &endpoint_id,
                    &ClientMessage::ClientShellFocus {
                        focused: self.focused,
                    },
                );
                self.schedule_health();
                let label = self.label(&endpoint_id);
                self.shared.publish(|outbox| {
                    let pending = outbox.endpoint(&EndpointId::from_client(&endpoint_id), &label);
                    pending.status = Some(EndpointStatus::Online);
                    pending.negotiation = Some(published);
                });
                self.sidebar_online(&endpoint_id, true);
            }
        }
    }

    fn publish_status(&mut self, id: &ClientEndpointId, status: EndpointStatus) {
        let label = self.label(id);
        let online = status == EndpointStatus::Online;
        self.shared.publish(|outbox| {
            outbox.endpoint(&EndpointId::from_client(id), &label).status = Some(status);
        });
        self.sidebar_online(id, online);
    }

    fn reader_event(&mut self, event: ClientLoopEvent) {
        match event {
            ClientLoopEvent::ServerMessage {
                endpoint_id,
                generation,
                message,
            } => {
                if !self.registry.accepts(&endpoint_id, generation) {
                    return;
                }
                let now = Instant::now();
                self.registry.received(&endpoint_id, generation, now);
                self.supervisors.received(&endpoint_id, generation);
                self.server_message(endpoint_id, generation, *message);
            }
            ClientLoopEvent::ServerDisconnected {
                endpoint_id,
                generation,
            } => {
                if !self.registry.accepts(&endpoint_id, generation) {
                    return;
                }
                self.lost(&endpoint_id, generation, "the server closed the connection");
            }
            _ => {}
        }
    }

    fn lost(&mut self, endpoint_id: &ClientEndpointId, generation: u64, reason: &str) {
        self.registry.disconnect(endpoint_id);
        self.streaming.remove(&endpoint_id.storage_key());
        self.boot_ids.remove(endpoint_id);
        self.responses.retain(|(id, _), _| id != endpoint_id);
        if self
            .supervisors
            .disconnected(endpoint_id, generation, Instant::now())
        {
            self.publish_status(endpoint_id, EndpointStatus::Reconnecting(reason.to_owned()));
        }
        self.schedule_health();
    }

    fn handle_failures(&mut self) {
        for failure in self.registry.take_failures() {
            if !self
                .registry
                .accepts(&failure.endpoint_id, failure.generation)
            {
                continue;
            }
            let message = failure.message.clone();
            self.lost(&failure.endpoint_id, failure.generation, &message);
        }
    }

    fn server_message(
        &mut self,
        endpoint_id: ClientEndpointId,
        generation: u64,
        message: ServerMessage,
    ) {
        let id = EndpointId::from_client(&endpoint_id);
        let label = self.label(&endpoint_id);
        match message {
            ServerMessage::ClientShellSnapshot(snapshot) => {
                self.snapshot(&endpoint_id, generation, snapshot);
            }
            // Generation 1 sends snapshots, health pongs and optional
            // projections as named control messages.
            ServerMessage::EndpointControl { kind, data } => {
                match super::endpoint::decode_endpoint_control(&kind, &data) {
                    Ok(super::endpoint::EndpointControlMessage::Snapshot(snapshot)) => {
                        self.snapshot(&endpoint_id, generation, snapshot);
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(endpoint = %endpoint_id.storage_key(), %error, "invalid endpoint control");
                    }
                }
            }
            ServerMessage::PaneSurface(surface) => {
                if endpoint_id != self.active {
                    return;
                }
                self.shared
                    .publish(|outbox| outbox.surface_full(&id, &label, surface));
            }
            ServerMessage::PaneSurfacePatch(patch) => {
                if endpoint_id != self.active {
                    return;
                }
                self.shared.publish(|outbox| {
                    outbox.surface_patch(&id, &label, &patch);
                });
            }
            ServerMessage::ClientShellEndpointResponseChunk {
                boot_id,
                request_id,
                final_chunk,
                data,
            } => self.response_chunk(&endpoint_id, &boot_id, request_id, final_chunk, data),
            ServerMessage::SemanticNotification(notification) => {
                self.shared.publish(|outbox| {
                    outbox.event(CoreEvent::Notification {
                        endpoint: id,
                        notification,
                    })
                });
            }
            ServerMessage::ClientShellError { message } => {
                self.shared.publish(|outbox| {
                    outbox.event(CoreEvent::Error {
                        endpoint: id,
                        message,
                    })
                });
            }
            ServerMessage::ClientShellKeyboardReportAll { enabled } => {
                self.shared.publish(|outbox| {
                    outbox.event(CoreEvent::KeyboardReportAll {
                        endpoint: id,
                        enabled,
                    })
                });
            }
            ServerMessage::TerminalBell { count } => {
                self.shared.publish(|outbox| {
                    outbox.event(CoreEvent::Bell {
                        endpoint: id,
                        count,
                    })
                });
            }
            ServerMessage::WindowTitle { title } => {
                self.shared.publish(|outbox| {
                    outbox.event(CoreEvent::Title {
                        endpoint: id,
                        title,
                    })
                });
            }
            ServerMessage::Clipboard { data } => {
                if let Some(data) = super::clipboard_forwarding::decode_clipboard_payload(&data) {
                    self.shared.publish(|outbox| {
                        outbox.event(CoreEvent::Clipboard { endpoint: id, data })
                    });
                }
            }
            // Health pongs only count as traffic; graphics and direct-terminal
            // messages are not part of the embed surface yet.
            _ => {}
        }
    }

    fn snapshot(
        &mut self,
        endpoint_id: &ClientEndpointId,
        generation: u64,
        snapshot: Box<ClientShellSnapshot>,
    ) {
        self.registry.mark_ready(endpoint_id, generation);
        self.boot_ids
            .insert(endpoint_id.clone(), snapshot.boot_id.clone());
        let id = EndpointId::from_client(endpoint_id);
        let label = self.label(endpoint_id);
        if let Some(sidebar) = self.sidebar.as_mut() {
            sidebar
                .snapshots
                .insert(endpoint_id.clone(), snapshot.clone());
        }
        self.shared.publish(|outbox| {
            outbox.endpoint(&id, &label).snapshot = Some(snapshot);
        });
        self.refresh_sidebar();
        self.ensure_streaming(endpoint_id);
    }

    fn response_chunk(
        &mut self,
        endpoint_id: &ClientEndpointId,
        boot_id: &str,
        request_id: String,
        final_chunk: bool,
        data: Vec<u8>,
    ) {
        if self.boot_ids.get(endpoint_id).map(String::as_str) != Some(boot_id) {
            return;
        }
        let key = (endpoint_id.clone(), request_id);
        let Some((_, buffer)) = self.responses.get_mut(&key) else {
            return;
        };
        buffer.extend(data);
        if !final_chunk {
            return;
        }
        let Some((id, buffer)) = self.responses.remove(&key) else {
            return;
        };
        if id.0 == 0 {
            return; // an internal request, such as turning surface streaming on
        }
        let result = super::endpoint_commands::parse_response(&key.1, &buffer)
            .map(Box::new)
            .map_err(|error| EndpointError {
                code: error.code,
                message: error.message,
            });
        let endpoint = EndpointId::from_client(endpoint_id);
        self.shared.publish(|outbox| {
            outbox.event(CoreEvent::Response {
                id,
                endpoint,
                result,
            })
        });
    }

    /// Sends a JSON request. `id` 0 marks an internal request whose response
    /// is consumed silently.
    fn send_request(
        &mut self,
        endpoint_id: &ClientEndpointId,
        id: RequestId,
        method: crate::api::schema::Method,
    ) -> Result<(), String> {
        let Some(boot_id) = self.boot_ids.get(endpoint_id).cloned() else {
            return Err(format!("{} is not connected", self.label(endpoint_id)));
        };
        let wire_id = if id.0 == 0 {
            self.next_internal_request += 1;
            format!("core-internal-{}", self.next_internal_request)
        } else {
            format!("core-{}", id.0)
        };
        let request = serde_json::to_string(&crate::api::schema::Request {
            id: wire_id.clone(),
            method,
        })
        .map_err(|error| error.to_string())?;
        let message = ClientMessage::ClientShellEndpointRequest { boot_id, request };
        if self.registry.send_to(endpoint_id, &message)
            != super::endpoint::EndpointSendOutcome::Sent
        {
            return Err(format!("{} is not connected", self.label(endpoint_id)));
        }
        self.responses
            .insert((endpoint_id.clone(), wire_id), (id, Vec::new()));
        Ok(())
    }

    /// Turns surface streaming on for the active endpoint once its snapshot
    /// has given the boot id that requests are addressed to.
    fn ensure_streaming(&mut self, endpoint_id: &ClientEndpointId) {
        if endpoint_id != &self.active || self.streaming.contains(&endpoint_id.storage_key()) {
            return;
        }
        if !self.boot_ids.contains_key(endpoint_id) {
            return;
        }
        let resize = self.resize_message();
        if self.registry.send_to(endpoint_id, &resize) != super::endpoint::EndpointSendOutcome::Sent
        {
            return;
        }
        let method = crate::api::schema::Method::ClientShellSurfaceSet(
            crate::api::schema::ClientShellSurfaceSetParams { active: true },
        );
        if self.send_request(endpoint_id, RequestId(0), method).is_ok() {
            self.registry.set_surface_active(endpoint_id, true);
            self.streaming.insert(endpoint_id.storage_key());
        }
    }

    fn stop_streaming(&mut self, endpoint_id: &ClientEndpointId) {
        if !self.streaming.remove(&endpoint_id.storage_key()) {
            return;
        }
        let method = crate::api::schema::Method::ClientShellSurfaceSet(
            crate::api::schema::ClientShellSurfaceSetParams { active: false },
        );
        let _ = self.send_request(endpoint_id, RequestId(0), method);
        self.registry.set_surface_active(endpoint_id, false);
    }

    fn resize_message(&self) -> ClientMessage {
        ClientMessage::ClientShellResize {
            cell_width_px: self.options.cell_px.0,
            cell_height_px: self.options.cell_px.1,
            surface_size: self.surface_size(),
            pixel_mouse: self.options.cell_px.0 > 0 && self.options.cell_px.1 > 0,
        }
    }

    fn client_id(&self, id: &EndpointId) -> Option<ClientEndpointId> {
        self.labels
            .keys()
            .find(|candidate| candidate.storage_key() == id.0)
            .cloned()
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Request {
                id,
                endpoint,
                method,
            } => {
                let result = match self.client_id(&endpoint) {
                    Some(endpoint_id) => self.send_request(&endpoint_id, id, *method),
                    None => Err(format!("unknown endpoint {}", endpoint.0)),
                };
                if let Err(message) = result {
                    self.shared.publish(|outbox| {
                        outbox.event(CoreEvent::Response {
                            id,
                            endpoint,
                            result: Err(EndpointError {
                                code: Some("endpoint_unavailable".into()),
                                message,
                            }),
                        })
                    });
                }
            }
            Command::PaneInput {
                endpoint,
                pane_id,
                events,
            } => {
                if let Some(endpoint_id) = self.client_id(&endpoint) {
                    let _ = self.registry.send_to(
                        &endpoint_id,
                        &ClientMessage::ClientShellPaneInput { pane_id, events },
                    );
                }
            }
            Command::SetActive(endpoint) => {
                let Some(endpoint_id) = self.client_id(&endpoint) else {
                    return;
                };
                if endpoint_id == self.active {
                    return;
                }
                let previous = std::mem::replace(&mut self.active, endpoint_id.clone());
                self.stop_streaming(&previous);
                self.ensure_streaming(&endpoint_id);
            }
            Command::ResizeSurface(cols, rows) => {
                self.options.surface_size = (cols, rows);
                self.resend_resize();
            }
            Command::CellPx(width, height) => {
                self.options.cell_px = (width, height);
                self.resend_resize();
            }
            Command::Focus(focused) => {
                self.focused = focused;
                self.broadcast(&ClientMessage::ClientShellFocus { focused });
            }
            Command::HostTheme(update) => {
                self.broadcast(&ClientMessage::ClientShellHostTheme { update });
            }
            Command::Retry(endpoint) => {
                if let Some(endpoint_id) = self.client_id(&endpoint) {
                    self.supervisors.retry_now(&endpoint_id, Instant::now());
                }
            }
            Command::SidebarPrefs(prefs) => {
                match self.sidebar.as_mut() {
                    Some(sidebar) => sidebar.prefs = *prefs,
                    None => {
                        let mut sidebar = CoreSidebar::new(*prefs);
                        // Endpoints that already reported Online stay online;
                        // later snapshots fill in the model.
                        sidebar.online.extend(
                            self.order
                                .iter()
                                .filter(|id| self.boot_ids.contains_key(id))
                                .cloned(),
                        );
                        self.sidebar = Some(sidebar);
                    }
                }
                self.refresh_sidebar();
            }
            Command::Shutdown => {}
        }
    }

    fn resend_resize(&mut self) {
        let active = self.active.clone();
        if self.streaming.contains(&active.storage_key()) {
            let resize = self.resize_message();
            let _ = self.registry.send_to(&active, &resize);
        }
    }

    fn broadcast(&mut self, message: &ClientMessage) {
        for endpoint_id in self.labels.keys().cloned().collect::<Vec<_>>() {
            if self.registry.connection(&endpoint_id).is_some() {
                let _ = self.registry.send_to(&endpoint_id, message);
            }
        }
    }
}

fn core_status(status: ClientEndpointStatus, message: String) -> EndpointStatus {
    match status {
        ClientEndpointStatus::Connecting => EndpointStatus::Connecting,
        ClientEndpointStatus::Online => EndpointStatus::Online,
        ClientEndpointStatus::Reconnecting => EndpointStatus::Reconnecting(message),
        ClientEndpointStatus::Attention | ClientEndpointStatus::Disabled => {
            EndpointStatus::Attention(message)
        }
    }
}

async fn sleep_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
#[path = "core_tests.rs"]
mod tests;
