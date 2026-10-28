//! The sidebar, drawn natively from oasis's `SidebarModel`: machines, the spaces tree with
//! worktree groups, agents across machines, and the footer, with the same tokens and
//! styles the TUI shows.

use std::cell::{Cell, RefCell};
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
    /// Agents above the spaces, as the touch drawer shows them; a wide window keeps
    /// the spaces first.
    agents_first: Cell<bool>,
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
            agents_first: Cell::new(false),
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
        // A long press is a finger's right-click: it opens the row's menu.
        let hold = gtk::GestureLongPress::new();
        hold.set_touch_only(true);
        let weak = Rc::downgrade(&view);
        hold.connect_pressed(move |gesture, _, y| {
            let Some(view) = weak.upgrade() else { return };
            if let Some(row) = view.list.row_at_y(y as i32) {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                view.fire(&row, gtk::gdk::BUTTON_SECONDARY);
            }
        });
        view.list.add_controller(hold);
        view
    }

    /// Called with the clicked target, its row, and the mouse button.
    pub fn connect(&self, handler: impl Fn(&Target, Option<&gtk::ListBoxRow>, u32) + 'static) {
        self.handler.replace(Some(Box::new(handler)));
    }

    /// Puts the agents above the spaces, or back below them; true when that changed,
    /// so the caller knows to render again.
    pub fn set_agents_first(&self, agents_first: bool) -> bool {
        self.agents_first.replace(agents_first) != agents_first
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
        stalled: &[String],
        fg: crate::theme::Rgb,
    ) {
        self.list.remove_all();
        let mut targets = Vec::new();
        let agents_first = self.agents_first.get();
        let has_agents = !model.agents.is_empty();
        if agents_first && has_agents {
            self.agent_rows(model, active_endpoint, stalled, fg, &mut targets);
            self.divider(&mut targets);
        }
        self.space_rows(model, active_endpoint, stalled, &mut targets);
        if !agents_first && has_agents {
            self.divider(&mut targets);
            self.agent_rows(model, active_endpoint, stalled, fg, &mut targets);
        }
        self.targets.replace(targets);
    }

    /// The machines and their spaces, under a SPACES heading when there is one machine.
    /// A machine that stopped answering says so in yellow.
    fn space_rows(
        self: &Rc<Self>,
        model: &SidebarModel,
        active_endpoint: &str,
        stalled: &[String],
        targets: &mut Vec<Option<Target>>,
    ) {
        let several = model.machines.len() > 1;
        if !several {
            let row = self.heading("SPACES", targets);
            if model
                .machines
                .first()
                .is_some_and(|machine| stalled.contains(&machine.id))
            {
                if let Some(heading) = row.child() {
                    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                    row.set_child(None::<&gtk::Widget>);
                    heading.set_hexpand(true);
                    line.append(&heading);
                    line.append(&not_responding_label());
                    row.set_child(Some(&line));
                }
            }
        }
        for machine in &model.machines {
            if several {
                if machine.id != model.machines[0].id {
                    self.divider(targets);
                }
                let not_responding = stalled.contains(&machine.id);
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.append(&dot(machine_dot(machine.online, not_responding)));
                let label = gtk::Label::new(Some(if machine.local {
                    "this machine"
                } else {
                    &machine.label
                }));
                label.set_xalign(0.0);
                label.set_hexpand(true);
                label.add_css_class("machine");
                row.append(&label);
                if not_responding {
                    row.append(&not_responding_label());
                }
                let list_row = self.push(
                    row.upcast_ref(),
                    targets,
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
                    targets,
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
    }

    /// The AGENTS heading and a row per agent across machines.
    fn agent_rows(
        &self,
        model: &SidebarModel,
        active_endpoint: &str,
        stalled: &[String],
        fg: crate::theme::Rgb,
        targets: &mut Vec<Option<Target>>,
    ) {
        self.heading("AGENTS", targets);
        for agent in &model.agents {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.append(&dot(agent.status));
            let mut has_logo = false;
            if let Some(tool) = agent.agent {
                if let Some(texture) = crate::logos::texture(tool, fg) {
                    let logo = gtk::Image::from_paintable(Some(&texture));
                    logo.set_pixel_size(14);
                    logo.set_valign(gtk::Align::Center);
                    logo.add_css_class("agent-logo");
                    logo.set_tooltip_text(Some(&format!("{tool:?}")));
                    row.append(&logo);
                    has_logo = true;
                }
            }
            let rows = without_agent_name(&agent.rows, has_logo);
            row.append(&token_rows(&rows));
            let list_row = self.push(
                row.upcast_ref(),
                targets,
                Some(Target::Agent {
                    endpoint: agent.endpoint.clone(),
                    pane_id: agent.pane_id.clone(),
                }),
            );
            if agent.focused && agent.endpoint == active_endpoint {
                list_row.add_css_class("focused");
            }
            if agent.stale || stalled.contains(&agent.endpoint) {
                list_row.add_css_class("stale");
            }
        }
    }

    fn divider(&self, targets: &mut Vec<Option<Target>>) {
        let line = gtk::Separator::new(gtk::Orientation::Horizontal);
        line.add_css_class("section-divider");
        let row = self.push(line.upcast_ref(), targets, None);
        row.set_activatable(false);
        row.add_css_class("heading-row");
    }

    fn heading(&self, text: &str, targets: &mut Vec<Option<Target>>) -> gtk::ListBoxRow {
        let label = gtk::Label::new(Some(text));
        label.set_xalign(0.0);
        label.add_css_class("heading");
        let row = self.push(label.upcast_ref(), targets, None);
        row.set_activatable(false);
        row.add_css_class("heading-row");
        row
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

/// A machine's dot: green online, yellow when connected but not answering, red offline.
fn machine_dot(online: bool, not_responding: bool) -> AgentStatus {
    if not_responding {
        AgentStatus::Working
    } else if online {
        AgentStatus::Done
    } else {
        AgentStatus::Blocked
    }
}

fn not_responding_label() -> gtk::Label {
    let label = gtk::Label::new(Some("◌ not responding"));
    label.add_css_class("not-responding");
    label.set_halign(gtk::Align::End);
    label
}

/// The agent rows to draw beside a logo: the logo names the tool, so its name and
/// its one-cell mark (`agent_icon`) are left out of every row, and the title line
/// drops the tab. Rows that end up empty go too.
fn without_agent_name(rows: &[TokenRow], has_logo: bool) -> Vec<TokenRow> {
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            row.iter()
                .filter_map(|token| match &token.kind {
                    ResolvedTokenKind::Agent(_) if has_logo => None,
                    ResolvedTokenKind::Tab(_) if index == 0 => None,
                    ResolvedTokenKind::Activity(parts) if has_logo => {
                        let parts: Vec<ActivityPart> = parts
                            .iter()
                            .filter(|part| part.kind != ActivityKind::Agent)
                            .cloned()
                            .collect();
                        (!parts.is_empty()).then(|| ResolvedToken {
                            kind: ResolvedTokenKind::Activity(parts),
                            style: token.style,
                        })
                    }
                    _ => Some(token.clone()),
                })
                .collect::<TokenRow>()
        })
        .filter(|row| {
            row.iter()
                .any(|token| !matches!(token.kind, ResolvedTokenKind::StateIcon))
        })
        .collect()
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
        // The row's logo already names the tool.
        ActivityKind::Agent => return None,
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
    fn a_machine_that_stopped_answering_shows_yellow_not_red() {
        assert_eq!(machine_dot(false, true), AgentStatus::Working);
        assert_eq!(machine_dot(true, false), AgentStatus::Done);
        assert_eq!(machine_dot(false, false), AgentStatus::Blocked);
    }

    #[test]
    fn a_logo_replaces_the_agent_name_and_mark_in_every_row() {
        use oasis_core::embed::sidebar::SidebarTokenStyle;
        let token = |kind| ResolvedToken {
            kind,
            style: SidebarTokenStyle::default(),
        };
        let rows = vec![
            vec![
                token(ResolvedTokenKind::StateIcon),
                token(ResolvedTokenKind::Activity(vec![part(
                    ActivityKind::Agent,
                    1,
                    None,
                )])),
                token(ResolvedTokenKind::Workspace("oasis".into())),
                token(ResolvedTokenKind::Activity(vec![part(
                    ActivityKind::Subagents,
                    2,
                    None,
                )])),
            ],
            vec![token(ResolvedTokenKind::Agent("Claude".into()))],
            vec![token(ResolvedTokenKind::TerminalTitle(
                "fix the phone app".into(),
            ))],
        ];
        let kept = without_agent_name(&rows, true);
        assert_eq!(kept.len(), 2, "the name-only row goes");
        assert_eq!(kept[0].len(), 3, "the mark goes, the subagents stay");
        assert_eq!(kept[1], rows[2]);
        assert_eq!(
            without_agent_name(&rows, false).len(),
            3,
            "no logo keeps the name"
        );
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
