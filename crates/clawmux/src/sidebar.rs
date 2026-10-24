//! The sidebar, drawn natively from oasis's `SidebarModel`: machines, the spaces tree with
//! worktree groups, agents across machines, and the footer, with the same tokens and
//! styles the TUI shows.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{glib, pango};
use oasis_core::embed::sidebar::{ResolvedToken, ResolvedTokenKind, SidebarModel, TokenRow};
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

    pub fn render(self: &Rc<Self>, model: &SidebarModel, active_endpoint: &str) {
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
            row.append(&token_rows(&agent.rows));
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
            let markup: Vec<String> = parts
                .iter()
                .filter(|part| !part.text.trim().is_empty())
                .map(|part| {
                    let text = glib::markup_escape_text(&part.text);
                    match activity_color(part.color) {
                        Some(color) => format!("<span foreground=\"{color}\">{text}</span>"),
                        None => text.to_string(),
                    }
                })
                .collect();
            return (!markup.is_empty()).then(|| markup.join(" "));
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

/// An activity part's color as CSS, with named ANSI colors taken from the Omarchy palette.
fn activity_color(color: ratatui::style::Color) -> Option<String> {
    use ratatui::style::Color;
    let index = match color {
        Color::Rgb(r, g, b) => return Some(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::Reset => return None,
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
        Color::Indexed(index) => usize::from(index),
    };
    Some(crate::theme::Theme::load().palette[index].css())
}

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
