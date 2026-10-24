//! The sidebar, drawn natively from oasis's `SidebarModel`: machines, the spaces tree with
//! worktree groups, agents across machines, and the footer, with the same tokens and
//! styles the TUI shows.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{glib, pango};
use oasis_core::embed::sidebar::{
    ActivityKind, ActivityPart, ResolvedToken, ResolvedTokenKind, SidebarModel, TokenRow,
};
use oasis_core::embed::wire::AgentStatus;

/// What a sidebar row stands for when clicked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Machine {
        endpoint: String,
    },
    Space {
        endpoint: String,
        workspace_id: String,
    },
    Agent {
        endpoint: String,
        pane_id: String,
    },
    GroupToggle {
        endpoint: String,
        key: String,
        collapsed: bool,
    },
}

type Handler = Box<dyn Fn(&Target, Option<&gtk::ListBoxRow>, u32)>;

pub struct SidebarView {
    pub root: gtk::Box,
    list: gtk::ListBox,
    pub status: gtk::Label,
    targets: RefCell<Vec<Option<Target>>>,
    handler: RefCell<Option<Handler>>,
}

impl SidebarView {
    pub fn new() -> Rc<Self> {
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        // Clicking rows must not move keyboard focus off the terminal.
        list.set_focusable(false);
        list.set_can_focus(false);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_use_markup(true);
        status.add_css_class("clawmux-status");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("clawmux-sidebar");
        root.set_size_request(260, -1);
        root.append(&scroller);
        root.append(&status);
        let view = Rc::new(Self {
            root,
            list,
            status,
            targets: RefCell::new(Vec::new()),
            handler: RefCell::new(None),
        });
        let weak = Rc::downgrade(&view);
        view.list.connect_row_activated(move |_, row| {
            if let Some(view) = weak.upgrade() {
                view.fire(row, gtk::gdk::BUTTON_PRIMARY);
            }
        });
        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_SECONDARY);
        let weak = Rc::downgrade(&view);
        click.connect_pressed(move |_, _, _, y| {
            let Some(view) = weak.upgrade() else { return };
            if let Some(row) = view.list.row_at_y(y as i32) {
                view.fire(&row, gtk::gdk::BUTTON_SECONDARY);
            }
        });
        view.list.add_controller(click);
        view
    }

    /// Called with the clicked target, its row, and the mouse button.
    pub fn connect(&self, handler: impl Fn(&Target, Option<&gtk::ListBoxRow>, u32) + 'static) {
        self.handler.replace(Some(Box::new(handler)));
    }

    fn fire(&self, row: &gtk::ListBoxRow, button: u32) {
        let target = self
            .targets
            .borrow()
            .get(row.index() as usize)
            .cloned()
            .flatten();
        if let (Some(target), Some(handler)) = (target, self.handler.borrow().as_ref()) {
            handler(&target, Some(row), button);
        }
    }

    fn fire_target(&self, target: Target) {
        if let Some(handler) = self.handler.borrow().as_ref() {
            handler(&target, None, gtk::gdk::BUTTON_PRIMARY);
        }
    }

    pub fn render(
        self: &Rc<Self>,
        model: &SidebarModel,
        active_endpoint: &str,
        fg: crate::theme::Rgb,
    ) {
        self.list.remove_all();
        let mut targets = Vec::new();
        let several = model.machines.len() > 1;
        if !several {
            self.heading("SPACES", &mut targets);
        }
        for machine in &model.machines {
            if several {
                if machine.id != model.machines[0].id {
                    self.divider(&mut targets);
                }
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.append(&dot(if machine.online {
                    AgentStatus::Done
                } else {
                    AgentStatus::Blocked
                }));
                let label = gtk::Label::new(Some(if machine.local {
                    "this machine"
                } else {
                    &machine.label
                }));
                label.set_xalign(0.0);
                label.set_hexpand(true);
                label.add_css_class("machine");
                row.append(&label);
                let list_row = self.push(
                    row.upcast_ref(),
                    &mut targets,
                    Some(Target::Machine {
                        endpoint: machine.id.clone(),
                    }),
                );
                if machine.id == active_endpoint {
                    list_row.add_css_class("active-machine");
                }
            }
            for space in &machine.spaces {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                if several {
                    row.set_margin_start(10);
                }
                row.append(&dot(space.status));
                let number = gtk::Label::new(Some(&space.number.to_string()));
                number.add_css_class("number");
                row.append(&number);
                if space.indented {
                    let branch =
                        gtk::Label::new(Some(if space.last_child { "└" } else { "├" }));
                    branch.add_css_class("meta");
                    row.append(&branch);
                }
                row.append(&token_rows(&space.rows));
                if let Some(group) = &space.group {
                    let toggle =
                        gtk::Button::with_label(if group.collapsed { "▸" } else { "▾" });
                    toggle.add_css_class("flat");
                    toggle.add_css_class("group-toggle");
                    toggle.set_valign(gtk::Align::Center);
                    let weak = Rc::downgrade(self);
                    let target = Target::GroupToggle {
                        endpoint: machine.id.clone(),
                        key: group.key.clone(),
                        collapsed: group.collapsed,
                    };
                    toggle.connect_clicked(move |_| {
                        if let Some(view) = weak.upgrade() {
                            view.fire_target(target.clone());
                        }
                    });
                    row.append(&toggle);
                }
                let list_row = self.push(
                    row.upcast_ref(),
                    &mut targets,
                    Some(Target::Space {
                        endpoint: machine.id.clone(),
                        workspace_id: space.workspace_id.clone(),
                    }),
                );
                if space.focused && machine.id == active_endpoint {
                    list_row.add_css_class("focused");
                }
            }
        }
        if !model.agents.is_empty() {
            self.divider(&mut targets);
            self.heading("AGENTS", &mut targets);
        }
        for agent in &model.agents {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.append(&dot(agent.status));
            if let Some(tool) = agent.agent {
                if let Some(texture) = crate::logos::texture(tool, fg) {
                    let logo = gtk::Image::from_paintable(Some(&texture));
                    logo.set_pixel_size(14);
                    logo.set_valign(gtk::Align::Center);
                    logo.add_css_class("agent-logo");
                    logo.set_tooltip_text(Some(&format!("{tool:?}")));
                    row.append(&logo);
                }
            }
            // The logo names the tool, so the title line keeps only the space name.
            let rows: Vec<TokenRow> = agent
                .rows
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    if index > 0 {
                        return row.clone();
                    }
                    row.iter()
                        .filter(|token| {
                            !matches!(
                                token.kind,
                                ResolvedTokenKind::Agent(_) | ResolvedTokenKind::Tab(_)
                            )
                        })
                        .cloned()
                        .collect()
                })
                .collect();
            row.append(&token_rows(&rows));
            let list_row = self.push(
                row.upcast_ref(),
                &mut targets,
                Some(Target::Agent {
                    endpoint: agent.endpoint.clone(),
                    pane_id: agent.pane_id.clone(),
                }),
            );
            if agent.focused && agent.endpoint == active_endpoint {
                list_row.add_css_class("focused");
            }
            if agent.stale {
                list_row.add_css_class("stale");
            }
        }
        self.targets.replace(targets);
    }

    fn divider(&self, targets: &mut Vec<Option<Target>>) {
        let line = gtk::Separator::new(gtk::Orientation::Horizontal);
        line.add_css_class("section-divider");
        let row = self.push(line.upcast_ref(), targets, None);
        row.set_activatable(false);
        row.add_css_class("heading-row");
    }

    fn heading(&self, text: &str, targets: &mut Vec<Option<Target>>) {
        let label = gtk::Label::new(Some(text));
        label.set_xalign(0.0);
        label.add_css_class("heading");
        let row = self.push(label.upcast_ref(), targets, None);
        row.set_activatable(false);
        row.add_css_class("heading-row");
    }

    fn push(
        &self,
        child: &gtk::Widget,
        targets: &mut Vec<Option<Target>>,
        target: Option<Target>,
    ) -> gtk::ListBoxRow {
        let row = gtk::ListBoxRow::new();
        row.set_focusable(false);
        row.set_child(Some(child));
        self.list.append(&row);
        targets.push(target);
        row
    }
}

/// A token row stack: the first row reads as the title, later rows as detail lines.
fn token_rows(rows: &[TokenRow]) -> gtk::Widget {
    let stack = gtk::Box::new(gtk::Orientation::Vertical, 0);
    stack.set_hexpand(true);
    for (index, row) in rows.iter().enumerate() {
        let markup: Vec<String> = row.iter().filter_map(token_markup).collect();
        if markup.is_empty() {
            continue;
        }
        let label = gtk::Label::new(None);
        label.set_markup(&markup.join(" "));
        label.set_xalign(0.0);
        label.set_ellipsize(pango::EllipsizeMode::End);
        if index > 0 {
            label.add_css_class("meta");
        }
        stack.append(&label);
    }
    stack.upcast()
}

fn token_markup(token: &ResolvedToken) -> Option<String> {
    #[allow(unreachable_patterns)]
    let text = match &token.kind {
        // The status dot already shows the state.
        ResolvedTokenKind::StateIcon => return None,
        ResolvedTokenKind::GitStatus { ahead, behind } => {
            let mut parts = Vec::new();
            if *ahead > 0 {
                parts.push(format!("↑{ahead}"));
            }
            if *behind > 0 {
                parts.push(format!("↓{behind}"));
            }
            parts.join(" ")
        }
        ResolvedTokenKind::Activity(parts) => {
            let markup: Vec<String> = parts.iter().filter_map(activity_markup).collect();
            return (!markup.is_empty()).then(|| markup.join("  "));
        }
        ResolvedTokenKind::StateText(text)
        | ResolvedTokenKind::Machine(text)
        | ResolvedTokenKind::Workspace(text)
        | ResolvedTokenKind::Tab(text)
        | ResolvedTokenKind::Pane(text)
        | ResolvedTokenKind::Agent(text)
        | ResolvedTokenKind::TerminalTitle(text)
        | ResolvedTokenKind::Branch(text)
        | ResolvedTokenKind::Custom(text) => text.clone(),
        _ => return None,
    };
    if text.trim().is_empty() {
        return None;
    }
    let mut attrs = String::new();
    if let Some(color) = token.style.fg {
        let (r, g, b) = color.rgb();
        attrs.push_str(&format!(" foreground=\"#{r:02x}{g:02x}{b:02x}\""));
    }
    if token.style.bold == Some(true) {
        attrs.push_str(" weight=\"bold\"");
    }
    if token.style.dim == Some(true) {
        attrs.push_str(" alpha=\"60%\"");
    }
    Some(format!(
        "<span{attrs}>{}</span>",
        glib::markup_escape_text(&text)
    ))
}

/// One background-activity part as an icon in its color with its count or schedule,
/// in the order oasis ranks them (goal, loop, shells/monitors/MCP, subagents).
fn activity_markup(part: &ActivityPart) -> Option<String> {
    #[allow(unreachable_patterns)]
    let icon = match part.kind {
        ActivityKind::Goal => "\u{f04fe}",      // target
        ActivityKind::Loop => "\u{f0450}",      // refresh
        ActivityKind::Shells => "\u{f018d}",    // console
        ActivityKind::Monitors => "\u{f0379}",  // monitor
        ActivityKind::Subagents => "\u{f06a9}", // robot
        ActivityKind::McpTasks => "\u{f0493}",  // cog
        _ => return Some(glib::markup_escape_text(&part.text).to_string()),
    };
    let (r, g, b) = part.color.rgb();
    let detail = match (&part.label, part.kind) {
        (Some(label), _) => format!(" {}", glib::markup_escape_text(label)),
        (None, ActivityKind::Goal | ActivityKind::Loop) => String::new(),
        (None, _) if part.count > 1 => format!(" {}", part.count),
        (None, _) => String::new(),
    };
    Some(format!(
        "<span foreground=\"#{r:02x}{g:02x}{b:02x}\"><span font_family=\"{ICON_FONT}\">{icon}</span>{detail}</span>"
    ))
}

/// Nerd Font icons, the terminal font Omarchy installs.
const ICON_FONT: &str = "JetBrainsMono Nerd Font";

pub fn dot(status: AgentStatus) -> gtk::Widget {
    let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    dot.add_css_class("dot");
    dot.add_css_class(match status {
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        _ => "idle",
    });
    dot.set_valign(gtk::Align::Center);
    dot.set_halign(gtk::Align::Center);
    dot.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;
    use oasis_core::embed::sidebar::SidebarTokenColor;

    fn part(kind: ActivityKind, count: u16, label: Option<&str>) -> ActivityPart {
        ActivityPart {
            kind,
            count,
            label: label.map(str::to_owned),
            text: String::new(),
            color: SidebarTokenColor::from_rgb(0x12, 0x34, 0x56),
        }
    }

    #[test]
    fn activity_parts_draw_as_colored_icons_with_counts_and_schedules() {
        let goal = activity_markup(&part(ActivityKind::Goal, 1, None)).unwrap();
        assert!(goal.contains("#123456") && goal.contains('\u{f04fe}') && !goal.contains(" 1"));
        let looped = activity_markup(&part(ActivityKind::Loop, 1, Some("22:42"))).unwrap();
        assert!(looped.contains('\u{f0450}') && looped.ends_with(" 22:42</span>"));
        let running = activity_markup(&part(ActivityKind::Loop, 1, None)).unwrap();
        assert!(running.ends_with("</span></span>"));
        let shells = activity_markup(&part(ActivityKind::Shells, 2, None)).unwrap();
        assert!(shells.contains('\u{f018d}') && shells.ends_with(" 2</span>"));
        let one = activity_markup(&part(ActivityKind::Subagents, 1, None)).unwrap();
        assert!(one.contains('\u{f06a9}') && one.ends_with("</span></span>"));
    }
}
