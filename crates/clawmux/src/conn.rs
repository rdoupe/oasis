//! One connection to the local oasis server over the frozen generation-1 endpoint protocol.
//!
//! A blocking reader thread forwards decoded messages to the GTK main loop through a
//! channel, so nothing polls: the GUI wakes only when the server sends something.

use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oasis_core::embed::wire::{
    self, ClientMessage, ClientPaneInputEvent, ClientShellSnapshot, ClientSurfaceSize,
    EndpointClientHello, EndpointServerWelcome, PaneSurfaceFrame, PaneSurfacePatch,
    SemanticNotification, ServerMessage,
};

pub enum Inbound {
    Snapshot(Box<ClientShellSnapshot>),
    Surface(Box<PaneSurfaceFrame>),
    Patch(Box<PaneSurfacePatch>),
    Notification(SemanticNotification),
    Error(String),
    Response { data: Vec<u8> },
    Title(Option<String>),
    Clipboard(String),
    Closed(String),
}

#[derive(Clone)]
pub struct Conn {
    writer: Arc<Mutex<UnixStream>>,
    boot_id: Arc<Mutex<String>>,
    next_request: Arc<AtomicU64>,
}

pub struct Geometry {
    pub cols: u16,
    pub rows: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
}

impl Conn {
    pub fn connect(geometry: &Geometry) -> io::Result<(Self, async_channel::Receiver<Inbound>)> {
        let path = wire::client_socket_path();
        let mut stream = connect_or_start(&path)?;
        handshake(&mut stream, geometry)?;
        let reader = stream.try_clone()?;
        let (sender, receiver) = async_channel::unbounded();
        let conn = Self {
            writer: Arc::new(Mutex::new(stream)),
            boot_id: Arc::new(Mutex::new(String::new())),
            next_request: Arc::new(AtomicU64::new(1)),
        };
        let boot_id = conn.boot_id.clone();
        std::thread::Builder::new()
            .name("clawmux-reader".into())
            .spawn(move || read_loop(reader, sender, boot_id))?;
        Ok((conn, receiver))
    }

    pub fn send(&self, message: &ClientMessage) {
        if let Ok(mut stream) = self.writer.lock() {
            if let Err(error) = wire::write_message(&mut *stream, message) {
                eprintln!("clawmux: send failed: {error}");
            }
        }
    }

    /// Sends one JSON API operation, e.g. `("tab.create", {"focus": true})`.
    pub fn request(&self, method: &str, params: serde_json::Value) -> String {
        let id = format!(
            "clawmux-{}",
            self.next_request.fetch_add(1, Ordering::Relaxed)
        );
        let request = serde_json::json!({ "id": id, "method": method, "params": params });
        let boot_id = self.boot_id.lock().map(|b| b.clone()).unwrap_or_default();
        self.send(&ClientMessage::ClientShellEndpointRequest {
            boot_id,
            request: request.to_string(),
        });
        id
    }

    pub fn pane_input(&self, pane_id: &str, events: Vec<ClientPaneInputEvent>) {
        if events.is_empty() {
            return;
        }
        self.send(&ClientMessage::ClientShellPaneInput {
            pane_id: pane_id.to_owned(),
            events,
        });
    }

    pub fn resize(&self, geometry: &Geometry) {
        self.send(&ClientMessage::ClientShellResize {
            cell_width_px: geometry.cell_width_px,
            cell_height_px: geometry.cell_height_px,
            surface_size: ClientSurfaceSize {
                cols: geometry.cols,
                rows: geometry.rows,
            },
            pixel_mouse: false,
        });
    }

    pub fn focus(&self, focused: bool) {
        self.send(&ClientMessage::ClientShellFocus { focused });
    }
}

fn connect_or_start(path: &Path) -> io::Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(path) {
        return Ok(stream);
    }
    start_server()?;
    // Startup only: wait for the new server's socket, then never poll again.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(error) if Instant::now() >= deadline => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("oasis server did not start at {}: {error}", path.display()),
                ))
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Starts the shared `oasis` server, the same one the TUI uses, detached from this process.
fn start_server() -> io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    let exe = std::env::var_os("CLAWMUX_OASIS").unwrap_or_else(|| "oasis".into());
    std::process::Command::new(&exe)
        .arg("server")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not start `{} server`: {error}", exe.to_string_lossy()),
            )
        })
}

fn handshake(stream: &mut UnixStream, geometry: &Geometry) -> io::Result<()> {
    let hello = EndpointClientHello {
        generation: wire::ENDPOINT_PROTOCOL_GENERATION,
        cell_width_px: geometry.cell_width_px,
        cell_height_px: geometry.cell_height_px,
        surface_size: ClientSurfaceSize {
            cols: geometry.cols,
            rows: geometry.rows,
        },
        pixel_mouse: false,
        direct_graphics: false,
        endpoint_keybindings: false,
        mouse_capture: true,
        surface_active: true,
        // Plain full surfaces and row patches only; the optional encodings save bandwidth
        // that a local socket does not need.
        surface_reuse: false,
        surface_delta: false,
        surface_scroll: false,
        snapshot_codecs: vec![wire::SNAPSHOT_CODEC_V1.into()],
        surface_codecs: vec![wire::SURFACE_CODEC_V1.into()],
        input_codecs: vec![wire::INPUT_CODEC_V1.into()],
        blob_codecs: vec![wire::BLOB_CODEC_V1.into()],
    };
    let message = ClientMessage::EndpointControl {
        kind: wire::ENDPOINT_HELLO_KIND.into(),
        data: serde_json::to_string(&hello).map_err(io::Error::other)?,
    };
    wire::write_message(stream, &message).map_err(framing)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let reply: ServerMessage = wire::read_message(stream, wire::MAX_FRAME_SIZE).map_err(framing)?;
    stream.set_read_timeout(None)?;
    let ServerMessage::EndpointControl { kind, data } = reply else {
        return Err(invalid("server does not speak the oasis endpoint protocol"));
    };
    if kind != wire::ENDPOINT_WELCOME_KIND {
        return Err(invalid("expected an endpoint welcome"));
    }
    let welcome: EndpointServerWelcome = serde_json::from_str(&data).map_err(io::Error::other)?;
    if let Some(error) = welcome.error {
        return Err(invalid(&format!("server refused the GUI: {}", error.message)));
    }
    if welcome.generation != wire::ENDPOINT_PROTOCOL_GENERATION {
        return Err(invalid("server endpoint generation is incompatible; update oasis"));
    }
    Ok(())
}

fn read_loop(
    mut stream: UnixStream,
    sender: async_channel::Sender<Inbound>,
    boot_id: Arc<Mutex<String>>,
) {
    let reason = loop {
        let message: ServerMessage = match wire::read_message(&mut stream, wire::MAX_FRAME_SIZE) {
            Ok(message) => message,
            Err(error) => break format!("connection to oasis closed: {error}"),
        };
        let inbound = match message {
            ServerMessage::EndpointControl { kind, data } => {
                if kind != wire::ENDPOINT_SNAPSHOT_KIND {
                    continue;
                }
                match serde_json::from_str::<ClientShellSnapshot>(&data) {
                    Ok(snapshot) => {
                        if let Ok(mut current) = boot_id.lock() {
                            current.clone_from(&snapshot.boot_id);
                        }
                        Inbound::Snapshot(Box::new(snapshot))
                    }
                    Err(error) => Inbound::Error(format!("unreadable snapshot: {error}")),
                }
            }
            ServerMessage::ClientShellSnapshot(snapshot) => {
                if let Ok(mut current) = boot_id.lock() {
                    current.clone_from(&snapshot.boot_id);
                }
                Inbound::Snapshot(snapshot)
            }
            ServerMessage::PaneSurface(surface) => Inbound::Surface(Box::new(surface)),
            ServerMessage::PaneSurfacePatch(patch) => Inbound::Patch(Box::new(patch)),
            ServerMessage::SemanticNotification(notification) => {
                Inbound::Notification(notification)
            }
            ServerMessage::ClientShellError { message } => Inbound::Error(message),
            ServerMessage::ClientShellEndpointResponseChunk {
                final_chunk: true,
                data,
                ..
            } => Inbound::Response { data },
            ServerMessage::WindowTitle { title } => Inbound::Title(title),
            ServerMessage::Clipboard { data } => Inbound::Clipboard(data),
            ServerMessage::ServerShutdown { reason } => {
                break reason.unwrap_or_else(|| "oasis server shut down".into())
            }
            _ => continue,
        };
        if sender.send_blocking(inbound).is_err() {
            return;
        }
    };
    let _ = sender.send_blocking(Inbound::Closed(reason));
}

fn framing(error: wire::FramingError) -> io::Error {
    io::Error::other(error.to_string())
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}
