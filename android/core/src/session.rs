//! One app session: the client core with its connections to saved machines, the
//! active machine's grid, and what the Kotlin screens read, as JSON.

use std::collections::BTreeMap;

use oasis_core::embed::sidebar::{
    ActivityPart, ResolvedTokenKind, SidebarModel, SidebarPrefs, TokenRow,
};
use oasis_core::embed::wire::{
    AgentStatus, ClientKeyCode, ClientKeyKind, ClientMouseKind, ClientMousePosition, Method,
};
use oasis_core::embed::{
    ClientCore, CoreEvent, CoreOptions, EndpointId, EndpointStatus, MachineSource, PaneInput,
    SavedMachine, SurfaceUpdate,
};
use serde_json::{json, Value};

use crate::frame::Palette;
use crate::grid::Grid;

pub struct Session {
    core: ClientCore,
    endpoints: BTreeMap<String, Endpoint>,
    active: Option<EndpointId>,
    pub grid: Grid,
    pub palette: Palette,
}

struct Endpoint {
    id: EndpointId,
    label: String,
    status: EndpointStatus,
}

impl Session {
    /// `machines` is a JSON list of `{"label", "target", "session"}`.
    pub fn start(machines: &str, cols: u16, rows: u16, cell_px: (u32, u32)) -> Result<Self, String> {
        let machines: Vec<Value> = serde_json::from_str(machines).map_err(|e| e.to_string())?;
        let machines = machines
            .iter()
            .filter_map(|m| {
                Some(SavedMachine {
                    label: m.get("label")?.as_str()?.to_owned(),
                    target: m.get("target")?.as_str()?.to_owned(),
                    session: m
                        .get("session")
                        .and_then(Value::as_str)
                        .unwrap_or("default")
                        .to_owned(),
                })
            })
            .collect();
        let core = ClientCore::start(CoreOptions {
            surface_size: (cols, rows),
            cell_px,
            mouse_capture: true,
            endpoint_keybindings: false,
            machines: MachineSource::Explicit(machines),
            local: false,
            server_exe: None,
            sidebar: Some(SidebarPrefs::load()),
        })
        .map_err(|e| e.to_string())?;
        Ok(Self {
            core,
            endpoints: BTreeMap::new(),
            active: None,
            grid: Grid::default(),
            palette: Palette::default(),
        })
    }

    pub fn wake_fd(&self) -> i32 {
        self.core.wake_fd()
    }

    /// Takes what changed: `{"machines"?, "agents"?, "surface", "notices", "clipboard"?}`.
    pub fn drain(&mut self) -> Value {
        let drained = self.core.drain();
        let mut surface = false;
        let mut machines_changed = false;
        for update in drained.endpoints {
            let key = update.id.as_str().to_owned();
            let endpoint = self.endpoints.entry(key).or_insert_with(|| {
                machines_changed = true;
                Endpoint {
                    id: update.id.clone(),
                    label: update.label.clone(),
                    status: EndpointStatus::Connecting,
                }
            });
            if let Some(status) = update.status {
                endpoint.status = status;
                machines_changed = true;
            }
            if self.active.is_none() {
                self.active = Some(update.id.clone());
                self.core.set_active(&update.id);
            }
            if self.active.as_ref() == Some(&update.id) {
                match update.surface {
                    Some(SurfaceUpdate::Full(frame)) => {
                        self.grid.set_full(*frame);
                        surface = true;
                    }
                    Some(SurfaceUpdate::Patch {
                        rows,
                        panes,
                        cursor,
                        ..
                    }) => {
                        self.grid.apply_rows(rows, panes, cursor);
                        surface = true;
                    }
                    None => {}
                }
            }
        }
        let mut notices = Vec::new();
        let mut clipboard = None;
        for event in drained.events {
            match event {
                CoreEvent::Error { endpoint, message } => {
                    notices.push(format!("{}: {message}", self.label(&endpoint)));
                }
                CoreEvent::Response {
                    endpoint,
                    result: Err(error),
                    ..
                } => notices.push(format!("{}: {}", self.label(&endpoint), error.message)),
                CoreEvent::Clipboard { data, .. } => {
                    clipboard = Some(String::from_utf8_lossy(&data).into_owned());
                }
                _ => {}
            }
        }
        let mut out = json!({ "surface": surface, "notices": notices });
        if machines_changed || drained.sidebar.is_some() {
            out["machines"] = self.machines_json(drained.sidebar.as_ref());
        }
        if let Some(model) = &drained.sidebar {
            out["agents"] = agents_json(model);
        }
        if let Some(text) = clipboard {
            out["clipboard"] = Value::String(text);
        }
        out
    }

    fn label(&self, id: &EndpointId) -> String {
        self.endpoints
            .get(id.as_str())
            .map_or_else(|| id.as_str().to_owned(), |e| e.label.clone())
    }

    fn machines_json(&self, model: Option<&SidebarModel>) -> Value {
        Value::Array(
            self.endpoints
                .values()
                .map(|endpoint| {
                    let (status, detail) = match &endpoint.status {
                        EndpointStatus::Connecting => ("connecting", String::new()),
                        EndpointStatus::Online => ("online", String::new()),
                        EndpointStatus::Reconnecting(why) => ("reconnecting", why.clone()),
                        EndpointStatus::Attention(why) => ("attention", why.clone()),
                    };
                    let mut machine = json!({
                        "id": endpoint.id.as_str(),
                        "label": endpoint.label,
                        "status": status,
                        "detail": detail,
                        "active": self.active.as_ref() == Some(&endpoint.id),
                    });
                    if let Some(entry) = model
                        .and_then(|m| m.machines.iter().find(|m| m.id == endpoint.id.as_str()))
                    {
                        machine["spaces"] = entry
                            .spaces
                            .iter()
                            .map(|space| {
                                json!({
                                    "id": space.workspace_id,
                                    "name": rows_text(&space.rows).first().cloned().unwrap_or_default(),
                                    "status": status_name(space.status),
                                    "focused": space.focused,
                                })
                            })
                            .collect();
                    }
                    machine
                })
                .collect(),
        )
    }

    fn endpoint(&self, key: &str) -> Option<EndpointId> {
        self.endpoints.get(key).map(|e| e.id.clone())
    }

    /// Makes `endpoint` the machine whose surface is shown.
    pub fn activate(&mut self, endpoint: &str) {
        let Some(id) = self.endpoint(endpoint) else { return };
        if self.active.as_ref() != Some(&id) {
            self.grid.clear();
            self.core.set_active(&id);
            self.active = Some(id);
        }
    }

    pub fn retry(&self, endpoint: &str) {
        if let Some(id) = self.endpoint(endpoint) {
            self.core.retry(&id);
        }
    }

    /// Sends one API operation to a machine, e.g. `("pane.focus", {"pane_id": ...})`.
    pub fn request(&mut self, endpoint: &str, method: &str, params: Value) -> Result<(), String> {
        let id = self.endpoint(endpoint).ok_or("unknown machine")?;
        let method: Method =
            serde_json::from_value(json!({ "method": method, "params": params }))
                .map_err(|error| format!("{method}: {error}"))?;
        self.core.request(&id, method);
        Ok(())
    }

    /// Shows an agent: its machine's surface, with its pane focused there.
    pub fn open_pane(&mut self, endpoint: &str, pane_id: &str) {
        self.activate(endpoint);
        let _ = self.request(endpoint, "pane.focus", json!({ "pane_id": pane_id }));
    }

    pub fn open_space(&mut self, endpoint: &str, workspace_id: &str) {
        self.activate(endpoint);
        let _ = self.request(
            endpoint,
            "workspace.focus",
            json!({ "workspace_id": workspace_id }),
        );
    }

    pub fn resize(&self, cols: u16, rows: u16, cell_px: (u32, u32)) {
        self.core.set_cell_px(cell_px.0, cell_px.1);
        self.core.resize_surface(cols, rows);
    }

    pub fn set_focus(&self, focused: bool) {
        self.core.set_focus(focused);
    }

    fn send(&self, events: Vec<PaneInput>) {
        let (Some(active), Some(pane)) = (&self.active, self.grid.focused_pane()) else {
            return;
        };
        self.core.pane_input(active, &pane.pane_id, events);
    }

    pub fn text(&self, text: &str) {
        if !text.is_empty() {
            self.send(vec![PaneInput::TextCommit(text.to_owned())]);
        }
    }

    pub fn paste(&self, text: &str) {
        self.send(vec![PaneInput::Paste(text.to_owned())]);
    }

    /// A key from the extra-keys row or a hardware keyboard; see [`key_code`].
    pub fn key(&self, code: i32, modifiers: u8) {
        let Some(code) = key_code(code) else { return };
        let generated_text = match code {
            ClientKeyCode::Char(c) if modifiers & !1 == 0 => Some(c.to_string()),
            _ => None,
        };
        let shifted_codepoint = match code {
            ClientKeyCode::Char(c) if modifiers & 1 != 0 => Some(c as u32),
            _ => None,
        };
        self.send(vec![PaneInput::Key {
            code,
            modifiers,
            kind: ClientKeyKind::Press,
            repeat_count: 1,
            shifted_codepoint,
            generated_text,
            tracks_release: false,
            physical_key_id: None,
            windows_record: None,
        }]);
    }

    /// Scrolls the pane under a cell: positive `lines` toward newer output.
    pub fn scroll(&self, lines: i32, col: u16, row: u16) {
        let (Some(active), Some(pane)) = (&self.active, self.grid.pane_at(col, row)) else {
            return;
        };
        let kind = if lines < 0 {
            ClientMouseKind::ScrollUp
        } else {
            ClientMouseKind::ScrollDown
        };
        let event = PaneInput::Mouse {
            kind,
            position: ClientMousePosition::Cell { column: col, row },
            geometry: None,
            modifiers: 0,
            lines: 1,
        };
        let count = lines.unsigned_abs().min(20) as usize;
        self.core
            .pane_input(active, &pane.pane_id, vec![event; count]);
    }

    /// Focuses the pane under a tapped cell when it is not focused yet.
    pub fn tap(&mut self, col: u16, row: u16) {
        let Some(pane) = self.grid.pane_at(col, row) else { return };
        if pane.focused {
            return;
        }
        let pane_id = pane.pane_id.clone();
        if let Some(active) = self.active.clone() {
            let _ = self.request(active.as_str(), "pane.focus", json!({ "pane_id": pane_id }));
        }
    }
}

/// The Kotlin key codes: named keys below 0x100, `0x100 + n` for F`n`, and a
/// character as `0x10000 + codepoint`.
pub fn key_code(code: i32) -> Option<ClientKeyCode> {
    Some(match code {
        0 => ClientKeyCode::Enter,
        1 => ClientKeyCode::Tab,
        2 => ClientKeyCode::Backspace,
        3 => ClientKeyCode::Esc,
        4 => ClientKeyCode::Left,
        5 => ClientKeyCode::Right,
        6 => ClientKeyCode::Up,
        7 => ClientKeyCode::Down,
        8 => ClientKeyCode::Home,
        9 => ClientKeyCode::End,
        10 => ClientKeyCode::PageUp,
        11 => ClientKeyCode::PageDown,
        12 => ClientKeyCode::Delete,
        13 => ClientKeyCode::Insert,
        14 => ClientKeyCode::BackTab,
        0x101..=0x118 => ClientKeyCode::F((code - 0x100) as u8),
        0x10000.. => ClientKeyCode::Char(char::from_u32((code - 0x10000) as u32)?),
        _ => return None,
    })
}

fn status_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Idle => "idle",
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Unknown => "unknown",
    }
}

fn agents_json(model: &SidebarModel) -> Value {
    model
        .agents
        .iter()
        .map(|agent| {
            let lines = rows_text(&agent.rows);
            json!({
                "endpoint": agent.endpoint,
                "machine": agent.machine_label,
                "pane": agent.pane_id,
                "space": agent.workspace_id,
                "status": status_name(agent.status),
                "agent": agent.agent.map(|a| format!("{a:?}")),
                "title": lines.first().cloned().unwrap_or_default(),
                "detail": lines[1.min(lines.len())..].join("  "),
                "focused": agent.focused,
                "stale": agent.stale,
            })
        })
        .collect()
}

/// Each token row as plain text, leaving out the state icon, which the app draws.
fn rows_text(rows: &[TokenRow]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            row.iter()
                .filter_map(|token| token_text(&token.kind))
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|line| !line.is_empty())
        .collect()
}

fn token_text(kind: &ResolvedTokenKind) -> Option<String> {
    Some(match kind {
        ResolvedTokenKind::StateIcon => return None,
        ResolvedTokenKind::StateText(text)
        | ResolvedTokenKind::Machine(text)
        | ResolvedTokenKind::Workspace(text)
        | ResolvedTokenKind::Tab(text)
        | ResolvedTokenKind::Pane(text)
        | ResolvedTokenKind::Agent(text)
        | ResolvedTokenKind::TerminalTitle(text)
        | ResolvedTokenKind::Branch(text)
        | ResolvedTokenKind::Custom(text) => text.clone(),
        ResolvedTokenKind::GitStatus { ahead, behind } => match (ahead, behind) {
            (0, 0) => return None,
            (a, 0) => format!("↑{a}"),
            (0, b) => format!("↓{b}"),
            (a, b) => format!("↑{a}↓{b}"),
        },
        ResolvedTokenKind::Activity(parts) => parts
            .iter()
            .map(|part: &ActivityPart| part.text.clone())
            .collect::<Vec<_>>()
            .join(" "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_codes_cover_named_function_and_character_keys() {
        assert_eq!(key_code(0), Some(ClientKeyCode::Enter));
        assert_eq!(key_code(3), Some(ClientKeyCode::Esc));
        assert_eq!(key_code(0x10C), Some(ClientKeyCode::F(12)));
        assert_eq!(key_code(0x10000 + 'c' as i32), Some(ClientKeyCode::Char('c')));
        assert_eq!(key_code(99), None);
    }
}
