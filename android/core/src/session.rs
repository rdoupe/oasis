//! One app session: the client core with its connections to saved machines, the
//! active machine's grid, and what the Kotlin screens read, as JSON.

use std::collections::BTreeMap;

use oasis_core::embed::sidebar::{
    ActivityKind, ActivityPart, Agent, CloseTarget, ResolvedTokenKind, SidebarModel, SidebarPrefs, TokenRow,
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
    /// The latest sidebar model: status-only updates still list each machine's spaces.
    sidebar: Option<SidebarModel>,
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
            sidebar: None,
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
        let sidebar_changed = drained.sidebar.is_some();
        if let Some(model) = drained.sidebar {
            out["agents"] = agents_json(&model);
            self.sidebar = Some(model);
        }
        if machines_changed || sidebar_changed {
            out["machines"] = self.machines_json();
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

    fn machines_json(&self) -> Value {
        let model = self.sidebar.as_ref();
        Value::Array(
            self.endpoints
                .values()
                .map(|endpoint| {
                    let (status, detail) = match &endpoint.status {
                        EndpointStatus::Connecting => ("connecting", String::new()),
                        EndpointStatus::Online => ("online", String::new()),
                        EndpointStatus::Reconnecting(why) => ("reconnecting", why.clone()),
                        EndpointStatus::Attention(why) => ("attention", why.clone()),
                        // Connected, but the machine stopped answering; pane input
                        // waits in the core and goes out once it answers.
                        EndpointStatus::NotResponding => ("not_responding", String::new()),
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
                                    "name": rows_text(&space.rows, false).first().cloned().unwrap_or_default(),
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

    /// The machine whose surface is shown.
    pub fn active_endpoint(&self) -> Option<&str> {
        self.active.as_ref().map(EndpointId::as_str)
    }

    /// The pane typing goes to on the shown machine.
    pub fn focused_pane(&self) -> Option<&str> {
        self.grid.focused_pane().map(|pane| pane.pane_id.as_str())
    }

    pub fn retry(&self, endpoint: &str) {
        if let Some(id) = self.endpoint(endpoint) {
            self.core.retry(&id);
        }
    }

    /// Sends one API operation to a machine, e.g. `("pane.focus", {"pane_id": ...})`.
    /// The server only takes them from a client showing its surface, so the
    /// machine becomes the shown one first.
    pub fn request(&mut self, endpoint: &str, method: &str, params: Value) -> Result<(), String> {
        let id = self.endpoint(endpoint).ok_or("unknown machine")?;
        self.activate(endpoint);
        if let Some(target) = close_target(method, &params) {
            // Hidden at once, as on the desktop, and back if the machine refuses.
            self.core.close(&id, target);
            return Ok(());
        }
        let method: Method =
            serde_json::from_value(json!({ "method": method, "params": params }))
                .map_err(|error| format!("{method}: {error}"))?;
        self.core.request(&id, method);
        Ok(())
    }

    /// Shows an agent: its machine's surface, with its pane focused there.
    pub fn open_pane(&mut self, endpoint: &str, pane_id: &str) {
        let _ = self.request(endpoint, "pane.focus", json!({ "pane_id": pane_id }));
    }

    pub fn open_space(&mut self, endpoint: &str, workspace_id: &str) {
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

/// What a `pane.close`, `tab.close` or `workspace.close` request closes.
fn close_target(method: &str, params: &Value) -> Option<CloseTarget> {
    let id = |key: &str| params.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(match method {
        "pane.close" => CloseTarget::Pane(id("pane_id")?),
        "tab.close" => CloseTarget::Tab(id("tab_id")?),
        "workspace.close" => CloseTarget::Workspaces(vec![id("workspace_id")?]),
        _ => return None,
    })
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

/// The agents the app draws its own logo for; their name and mark tokens are left
/// out of the text, as clawmux does.
fn logo(agent: Option<Agent>) -> Option<&'static str> {
    Some(match agent? {
        Agent::Claude => "claude",
        Agent::Codex => "codex",
        Agent::Cursor => "cursor",
        Agent::Grok => "grok",
        Agent::Gemini => "gemini",
        _ => return None,
    })
}

fn agents_json(model: &SidebarModel) -> Value {
    model
        .agents
        .iter()
        .map(|agent| {
            let logo = logo(agent.agent);
            let lines = card_lines(&agent.rows, logo.is_some(), agent.label.as_deref());
            json!({
                "endpoint": agent.endpoint,
                "machine": agent.machine_label,
                "pane": agent.pane_id,
                "space": agent.workspace_id,
                "status": status_name(agent.status),
                "agent": agent.agent.map(|a| format!("{a:?}")),
                "logo": logo,
                "label": agent.label,
                "title": lines.first().cloned().unwrap_or_default(),
                "detail": lines[1.min(lines.len())..].join("  "),
                "activity": activity_json(&agent.rows),
                "focused": agent.focused,
                "stale": agent.stale,
            })
        })
        .collect()
}

/// An agent card's lines, title first. Rows keep the order the sidebar config
/// lays them out, as the TUI and the tablet show them, so the title leads with
/// the space's name. The card names the machine on a line of its own, so the
/// machine is left out, and the title drops the tab, as the tablet's does. A
/// pane someone renamed shows that name first under the title.
fn card_lines(rows: &[TokenRow], logo: bool, label: Option<&str>) -> Vec<String> {
    let rows: Vec<TokenRow> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            row.iter()
                .filter(|token| match token.kind {
                    ResolvedTokenKind::Machine(_) => false,
                    ResolvedTokenKind::Tab(_) => index > 0,
                    _ => true,
                })
                .cloned()
                .collect()
        })
        .collect();
    let mut lines = rows_text(&rows, logo);
    if let Some(label) = label {
        lines.retain(|line| line != label);
        lines.insert(1.min(lines.len()), label.to_owned());
    }
    lines
}

/// Each token row as plain text, in the order the person's sidebar config lays it
/// out, leaving out the state icon, which the app draws, and with `logo` the
/// agent's name and mark, which its logo replaces.
fn rows_text(rows: &[TokenRow], logo: bool) -> Vec<String> {
    rows.iter()
        .map(|row| {
            row.iter()
                .filter_map(|token| token_text(&token.kind, logo))
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|line| !line.is_empty())
        .collect()
}

fn token_text(kind: &ResolvedTokenKind, logo: bool) -> Option<String> {
    Some(match kind {
        ResolvedTokenKind::StateIcon => return None,
        ResolvedTokenKind::Agent(_) if logo => return None,
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
        // Background work goes out as icons (activity_json); the agent's own mark
        // stays as text when the app has no logo for it.
        ResolvedTokenKind::Activity(parts) => parts
            .iter()
            .filter(|part: &&ActivityPart| !logo && part.kind == ActivityKind::Agent)
            .map(|part| part.text.clone())
            .collect::<Vec<_>>()
            .join(" "),
    })
}

/// The agent's background work as clawmux draws it: a Nerd Font icon per kind
/// (goal, loop, shells, monitors, subagents, MCP tasks) in the part's color, with
/// its schedule or a count above one. Kinds without an icon keep their text.
fn activity_json(rows: &[TokenRow]) -> Value {
    rows.iter()
        .flatten()
        .filter_map(|token| match &token.kind {
            ResolvedTokenKind::Activity(parts) => Some(parts),
            _ => None,
        })
        .flatten()
        .filter(|part| part.kind != ActivityKind::Agent)
        .map(|part| {
            let icon = match part.kind {
                ActivityKind::Goal => "\u{f04fe}",
                ActivityKind::Loop => "\u{f0450}",
                ActivityKind::Shells => "\u{f018d}",
                ActivityKind::Monitors => "\u{f0379}",
                ActivityKind::Subagents => "\u{f06a9}",
                ActivityKind::McpTasks => "\u{f0493}",
                _ => "",
            };
            let detail = match (&part.label, part.kind) {
                _ if icon.is_empty() => part.text.clone(),
                (Some(label), _) => label.clone(),
                (None, ActivityKind::Goal | ActivityKind::Loop) => String::new(),
                (None, _) if part.count > 1 => part.count.to_string(),
                (None, _) => String::new(),
            };
            let (r, g, b) = part.color.rgb();
            json!({
                "icon": icon,
                "detail": detail,
                "color": format!("#{r:02x}{g:02x}{b:02x}"),
            })
        })
        .collect()
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

    /// The app's pane and space menus send these; each must parse as the core's
    /// API method so the request is not dropped.
    #[test]
    fn the_menu_requests_parse_as_api_methods() {
        for (method, params) in [
            ("pane.close", json!({ "pane_id": "p_1" })),
            ("pane.rename", json!({ "pane_id": "p_1", "label": "review" })),
            ("tab.create", json!({ "workspace_id": "w_1", "focus": true })),
            ("workspace.rename", json!({ "workspace_id": "w_1", "label": "oasis" })),
            ("workspace.close", json!({ "workspace_id": "w_1" })),
        ] {
            let parsed: Result<Method, _> =
                serde_json::from_value(json!({ "method": method, "params": params }));
            assert!(parsed.is_ok(), "{method}: {parsed:?}");
        }
        let wrong: Result<Method, _> = serde_json::from_value(
            json!({ "method": "workspace.rename", "params": { "workspace_id": "w_1", "name": "x" } }),
        );
        assert!(wrong.is_err(), "a misnamed field is caught");
        assert_eq!(
            close_target("pane.close", &json!({ "pane_id": "p_1" })),
            Some(CloseTarget::Pane("p_1".into()))
        );
        assert_eq!(
            close_target("workspace.close", &json!({ "workspace_id": "w_1" })),
            Some(CloseTarget::Workspaces(vec!["w_1".into()]))
        );
        assert_eq!(close_target("pane.rename", &json!({ "pane_id": "p_1" })), None);
    }

    #[test]
    fn a_logo_replaces_the_agent_name_and_mark_but_keeps_the_rest() {
        use oasis_core::embed::sidebar::{ResolvedToken, SidebarTokenColor, SidebarTokenStyle};
        let token = |kind| ResolvedToken {
            kind,
            style: SidebarTokenStyle::default(),
        };
        let mark = ActivityPart {
            kind: ActivityKind::Agent,
            count: 1,
            label: None,
            text: "✳".into(),
            color: SidebarTokenColor::from_rgb(0, 0, 0),
        };
        let subagents = ActivityPart {
            kind: ActivityKind::Subagents,
            count: 2,
            label: None,
            text: "2 subagents".into(),
            color: SidebarTokenColor::from_rgb(0, 0, 0),
        };
        let rows = vec![
            vec![
                token(ResolvedTokenKind::StateIcon),
                token(ResolvedTokenKind::Activity(vec![mark.clone()])),
                token(ResolvedTokenKind::Workspace("oasis".into())),
                token(ResolvedTokenKind::Activity(vec![subagents])),
            ],
            vec![token(ResolvedTokenKind::TerminalTitle("fix the phone app".into()))],
            vec![token(ResolvedTokenKind::Agent("Claude".into()))],
        ];
        assert_eq!(
            rows_text(&rows, true),
            vec!["oasis".to_owned(), "fix the phone app".to_owned()]
        );
        assert_eq!(rows_text(&rows, false)[0], "✳ oasis");
        let activity = activity_json(&rows);
        assert_eq!(activity.as_array().map(Vec::len), Some(1), "the mark is not background work");
        assert_eq!(activity[0]["icon"], "\u{f06a9}");
        assert_eq!(activity[0]["detail"], "2");
    }

    #[test]
    fn a_card_is_titled_by_its_space_with_the_pane_name_under_it() {
        use oasis_core::embed::sidebar::{ResolvedToken, SidebarTokenStyle};
        let token = |kind| ResolvedToken {
            kind,
            style: SidebarTokenStyle::default(),
        };
        // The default agent rows: machine, space and tab, then the agent.
        let rows = vec![
            vec![
                token(ResolvedTokenKind::StateIcon),
                token(ResolvedTokenKind::Machine("server".into())),
                token(ResolvedTokenKind::Workspace("oasis".into())),
                token(ResolvedTokenKind::Tab("1".into())),
            ],
            vec![token(ResolvedTokenKind::Agent("Claude".into()))],
        ];
        assert_eq!(card_lines(&rows, false, None), vec!["oasis", "Claude"]);
        assert_eq!(card_lines(&rows, true, None), vec!["oasis"]);
        assert_eq!(card_lines(&rows, true, Some("review")), vec!["oasis", "review"]);
    }
}
