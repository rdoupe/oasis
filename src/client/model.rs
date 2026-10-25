//! Sidebar data derived from client-shell snapshots.
//!
//! The TUI sidebar renderers and embedded front ends (`embed::sidebar`) read
//! the same derivations: workspace grouping and roll-up status, resolved row
//! tokens, agent ordering across machines, tab labels, footer facts and
//! notification delivery. Nothing here draws; tokens carry neutral styles.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::api::schema::AgentStatus;
use crate::config::{
    AgentPanelSortConfig, AgentsSidebarConfig, SpacesSidebarConfig, ToastDelivery,
};
use crate::protocol::{
    ClientShellSnapshot, ClientShellTab, ClientShellWorkspace, SemanticNotificationKind,
};
pub use crate::ui::ResolvedToken;

/// One rendered sidebar line: tokens drawn left to right.
pub type TokenRow = Vec<ResolvedToken>;

/// Higher sorts first in priority order and wins workspace roll-ups.
pub(crate) fn status_priority(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Blocked => 4,
        AgentStatus::Done => 3,
        AgentStatus::Working => 2,
        AgentStatus::Idle => 1,
        AgentStatus::Unknown => 0,
    }
}

/// The sort rank in priority order, highest first: turns that ended unseen
/// (done, or a goal just met), needing you, idle, idle while subagents run,
/// busy, goals, and loops last. A goal or loop pane keeps its place whether it
/// is working, done or between runs, unless it needs you; a pane between
/// turns whose subagents run shows the robot, done or not. Each matches the
/// pane's state icon.
pub(crate) fn agent_sort_priority(status: AgentStatus, mode: Option<crate::ui::AgentMode>) -> u8 {
    use crate::ui::AgentMode;
    match (status, mode) {
        (AgentStatus::Blocked, _) => 6,
        (AgentStatus::Unknown, _) => 0,
        (AgentStatus::Idle | AgentStatus::Done, Some(AgentMode::GoalAchieved)) => 7,
        (_, Some(AgentMode::Loop)) => 1,
        (_, Some(AgentMode::Goal)) => 2,
        (AgentStatus::Working, _) => 3,
        (_, Some(AgentMode::Subagents)) => SUBAGENTS_RANK,
        (AgentStatus::Done, _) => 7,
        (AgentStatus::Idle, _) => IDLE_RANK,
    }
}

/// The rank of the idle tier in [`agent_sort_priority`].
const IDLE_RANK: u8 = 5;
/// Idle while subagents run, just below plain idle.
const SUBAGENTS_RANK: u8 = 4;

/// The sub-sort within idle panes: ones still running shells, monitors or
/// other background work go below plain idle ones, more work lower, and at the
/// same total more monitors lower. Other tiers don't sub-sort.
pub(crate) fn idle_background_load(
    status: AgentStatus,
    mode: Option<crate::ui::AgentMode>,
    agent: &crate::protocol::ClientShellAgent,
) -> (u32, u16) {
    if agent_sort_priority(status, mode) != IDLE_RANK {
        return (0, 0);
    }
    let count = |key: &str| {
        agent
            .tokens
            .iter()
            .find(|(token, _)| token == key)
            .and_then(|(_, value)| value.parse::<u16>().ok())
            .unwrap_or(0)
    };
    let monitors = count("oasis:monitors");
    let total = ["oasis:shells", "oasis:mcp_tasks", "oasis:tasks"]
        .into_iter()
        .map(|key| u32::from(count(key)))
        .sum::<u32>()
        + u32::from(monitors);
    (total, monitors)
}

/// The last sort key in priority order: the most recent change first, except
/// among idle panes (robot ones too), where the one idle longest comes first,
/// so going back to the top idle pane picks up the oldest.
pub(crate) fn recency_rank(
    status: AgentStatus,
    mode: Option<crate::ui::AgentMode>,
    recency: u64,
) -> u64 {
    if matches!(
        agent_sort_priority(status, mode),
        IDLE_RANK | SUBAGENTS_RANK
    ) {
        recency
    } else {
        u64::MAX - recency
    }
}

/// The goal, loop or subagents driving a pane, from its wire tokens.
pub(crate) fn agent_mode(
    agent: &crate::protocol::ClientShellAgent,
) -> Option<crate::ui::AgentMode> {
    crate::ui::AgentMode::from_keys(|key| agent.tokens.iter().any(|(token, _)| token == key))
}

pub(crate) fn status_text(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working => "working",
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Idle => "idle",
        AgentStatus::Unknown => "unknown",
    }
}

/// The agent row's state text when the agent sets no label of its own.
pub(crate) fn sidebar_status_text(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Blocked => "blocked",
        AgentStatus::Done => "done",
        AgentStatus::Working => "working",
        AgentStatus::Idle | AgentStatus::Unknown => "idle",
    }
}

/// A workspace's place in the sidebar list: worktree children are indented
/// under their parent repository.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkspaceEntry {
    /// Index into the snapshot's workspaces.
    pub index: usize,
    pub indented: bool,
    pub last_child: bool,
}

/// Orders workspaces for the sidebar, nesting linked worktrees under their
/// parent and hiding the children of collapsed groups (except a focused one).
pub(crate) fn workspace_entries(
    snapshot: &ClientShellSnapshot,
    collapsed_groups: &HashSet<String>,
) -> Vec<WorkspaceEntry> {
    let mut members = HashMap::<&str, Vec<usize>>::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        if let Some(worktree) = &workspace.worktree {
            members.entry(&worktree.key).or_default().push(index);
        }
    }
    let grouped = members
        .iter()
        .filter(|(_, indices)| {
            indices.iter().any(|index| {
                snapshot.workspaces[*index]
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| worktree.is_linked_worktree)
            }) && indices.iter().any(|index| {
                snapshot.workspaces[*index]
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| !worktree.is_linked_worktree)
            })
        })
        .map(|(key, _)| *key)
        .collect::<HashSet<_>>();
    let mut emitted = HashSet::<&str>::new();
    let mut entries = Vec::new();
    for (index, workspace) in snapshot.workspaces.iter().enumerate() {
        let Some(worktree) = workspace
            .worktree
            .as_ref()
            .filter(|worktree| grouped.contains(worktree.key.as_str()))
        else {
            entries.push(WorkspaceEntry {
                index,
                indented: false,
                last_child: false,
            });
            continue;
        };
        if !emitted.insert(&worktree.key) {
            continue;
        }
        let Some(group_members) = members.get(worktree.key.as_str()) else {
            continue;
        };
        for parent in group_members.iter().copied().filter(|member| {
            snapshot.workspaces[*member]
                .worktree
                .as_ref()
                .is_some_and(|worktree| !worktree.is_linked_worktree)
        }) {
            entries.push(WorkspaceEntry {
                index: parent,
                indented: false,
                last_child: false,
            });
        }
        if collapsed_groups.contains(&worktree.key) {
            if let Some(active) = group_members.iter().copied().find(|member| {
                let workspace = &snapshot.workspaces[*member];
                workspace.focused
                    && workspace
                        .worktree
                        .as_ref()
                        .is_some_and(|worktree| worktree.is_linked_worktree)
            }) {
                entries.push(WorkspaceEntry {
                    index: active,
                    indented: true,
                    last_child: true,
                });
            }
            continue;
        }
        let children = group_members
            .iter()
            .copied()
            .filter(|member| {
                snapshot.workspaces[*member]
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| worktree.is_linked_worktree)
            })
            .collect::<Vec<_>>();
        for (child_index, child) in children.iter().enumerate() {
            entries.push(WorkspaceEntry {
                index: *child,
                indented: true,
                last_child: child_index + 1 == children.len(),
            });
        }
    }
    entries
}

/// The group key a parent repository workspace toggles, when it has linked
/// worktree children.
pub(crate) fn parent_group_key(snapshot: &ClientShellSnapshot, index: usize) -> Option<String> {
    let workspace = snapshot.workspaces.get(index)?;
    let worktree = workspace.worktree.as_ref()?;
    if worktree.is_linked_worktree {
        return None;
    }
    snapshot
        .workspaces
        .iter()
        .any(|candidate| {
            candidate.worktree.as_ref().is_some_and(|candidate| {
                candidate.key == worktree.key && candidate.is_linked_worktree
            })
        })
        .then(|| worktree.key.clone())
}

/// Whether closing `workspace` closes a whole worktree group.
pub(crate) fn workspace_close_is_group(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
) -> bool {
    let Some(worktree) = workspace
        .worktree
        .as_ref()
        .filter(|worktree| !worktree.is_linked_worktree)
    else {
        return false;
    };
    let mut has_child = false;
    for member in &snapshot.workspaces {
        if member.workspace_id == workspace.workspace_id {
            continue;
        }
        if let Some(candidate) = member
            .worktree
            .as_ref()
            .filter(|candidate| candidate.key == worktree.key)
        {
            if !candidate.is_linked_worktree {
                return false;
            }
            has_child = true;
        }
    }
    has_child
}

/// A collapsed parent shows the most urgent status of its hidden children.
pub(crate) fn displayed_workspace_status(
    snapshot: &ClientShellSnapshot,
    workspace: &ClientShellWorkspace,
    collapsed_groups: &HashSet<String>,
) -> AgentStatus {
    let Some(worktree) = workspace
        .worktree
        .as_ref()
        .filter(|worktree| !worktree.is_linked_worktree)
    else {
        return workspace.agent_status;
    };
    if !collapsed_groups.contains(&worktree.key) {
        return workspace.agent_status;
    }
    snapshot
        .workspaces
        .iter()
        .filter(|candidate| {
            candidate.worktree.as_ref().is_some_and(|member| {
                member.key == worktree.key
                    && (member.is_linked_worktree
                        || candidate.workspace_id == workspace.workspace_id)
            })
        })
        .map(|candidate| candidate.agent_status)
        .max_by_key(|status| status_priority(*status))
        .unwrap_or(workspace.agent_status)
}

/// The configured token rows for one workspace.
pub(crate) fn workspace_rows(
    workspace: &ClientShellWorkspace,
    status: AgentStatus,
    indented: bool,
    config: &SpacesSidebarConfig,
) -> Vec<TokenRow> {
    let label = if indented && !workspace.custom_label {
        workspace
            .branch
            .as_deref()
            .and_then(|branch| branch.strip_prefix("worktree/").or(Some(branch)))
            .unwrap_or(&workspace.label)
    } else {
        &workspace.label
    };
    let token_values = workspace.tokens.iter().cloned().collect::<HashMap<_, _>>();
    crate::ui::sidebar_space_rows(
        config,
        crate::ui::SpaceTokenContext {
            workspace: label,
            branch: workspace.branch.as_deref(),
            state_text: status_text(status),
            ahead_behind: workspace.git_ahead_behind,
            tokens: &token_values,
            suppress_git_details: indented,
        },
    )
}

/// One agent pane's sidebar entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRow {
    pub pane_id: String,
    pub status: AgentStatus,
    /// A goal or loop driving the pane, which colors its state icon.
    pub mode: Option<crate::ui::AgentMode>,
    pub focused: bool,
    pub rows: Vec<TokenRow>,
}

/// One snapshot's agent panes in sidebar order: the server's agent view
/// order when one is set, otherwise snapshot order, or most urgent first.
pub(crate) fn ordered_agent_pane_ids(
    snapshot: &ClientShellSnapshot,
    sort: AgentPanelSortConfig,
) -> Vec<String> {
    if snapshot.agent_view_label.is_some() {
        return snapshot
            .agent_order
            .iter()
            .filter(|pane_id| {
                snapshot
                    .agents
                    .iter()
                    .any(|agent| agent.pane_id == pane_id.as_str())
            })
            .cloned()
            .collect();
    }
    let mut agents = snapshot.agents.iter().collect::<Vec<_>>();
    if sort == AgentPanelSortConfig::Priority {
        agents.sort_by_key(|agent| {
            let mode = agent_mode(agent);
            (
                std::cmp::Reverse(agent_sort_priority(agent.agent_status, mode)),
                idle_background_load(agent.agent_status, mode, agent),
                recency_rank(agent.agent_status, mode, agent.state_change_seq),
            )
        });
    }
    agents
        .into_iter()
        .map(|agent| agent.pane_id.clone())
        .collect()
}

/// Resolves one agent pane's configured token rows. `machine` names the
/// machine for remote agents in a combined list.
pub(crate) fn agent_row(
    snapshot: &ClientShellSnapshot,
    pane_id: &str,
    config: &AgentsSidebarConfig,
    machine: Option<&str>,
) -> Option<AgentRow> {
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| agent.pane_id == pane_id)?;
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == agent.workspace_id)?;
    let tab = snapshot.tabs.iter().find(|tab| tab.tab_id == agent.tab_id);
    let pane = snapshot
        .panes
        .iter()
        .find(|pane| pane.pane_id == agent.pane_id);
    let tab_count = snapshot
        .tabs
        .iter()
        .filter(|candidate| candidate.workspace_id == agent.workspace_id)
        .count();
    let tab_label = tab
        .filter(|tab| tab_count > 1 || tab.custom_label)
        .map(|tab| tab.label.as_str());
    let agent_label = agent
        .display_agent
        .as_deref()
        .or(agent.name.as_deref())
        .or(agent.agent.as_deref())
        .or(agent.title.as_deref());
    let labels = agent
        .state_labels
        .iter()
        .cloned()
        .collect::<HashMap<_, _>>();
    let tokens = agent.tokens.iter().cloned().collect::<HashMap<_, _>>();
    let state_text = labels
        .get(status_text(agent.agent_status))
        .map(String::as_str)
        .unwrap_or_else(|| sidebar_status_text(agent.agent_status));
    let canonical_agent = agent
        .agent
        .as_deref()
        .and_then(crate::detect::parse_agent_label);
    let rows = crate::ui::sidebar_agent_rows(
        config,
        crate::ui::AgentTokenContext {
            machine,
            workspace: &workspace.label,
            tab: tab_label,
            pane: agent
                .title
                .as_deref()
                .or_else(|| pane.and_then(|pane| pane.label.as_deref())),
            agent_label,
            terminal_title: agent.terminal_title.as_deref(),
            terminal_title_stripped: agent.terminal_title_stripped.as_deref(),
            canonical_agent,
            tokens: &tokens,
        },
        state_text,
    );
    Some(AgentRow {
        pane_id: agent.pane_id.clone(),
        status: agent.agent_status,
        mode: crate::ui::AgentMode::from_tokens(&tokens),
        focused: agent.focused,
        rows,
    })
}

/// All of one snapshot's agent rows in sidebar order.
pub(crate) fn agent_rows(
    snapshot: &ClientShellSnapshot,
    config: &AgentsSidebarConfig,
    sort: AgentPanelSortConfig,
    machine: Option<&str>,
) -> Vec<AgentRow> {
    ordered_agent_pane_ids(snapshot, sort)
        .into_iter()
        .filter_map(|pane_id| agent_row(snapshot, &pane_id, config, machine))
        .collect()
}

/// The priority order of a combined, multi-machine agent list: live
/// machines first, then most urgent status, idle panes with less background
/// work first ([`idle_background_load`]), then [`recency_rank`].
pub(crate) fn combined_priority_key(
    stale: bool,
    status: AgentStatus,
    mode: Option<crate::ui::AgentMode>,
    load: (u32, u16),
    recency: u64,
) -> (bool, std::cmp::Reverse<u8>, (u32, u16), u64) {
    (
        stale,
        std::cmp::Reverse(agent_sort_priority(status, mode)),
        load,
        recency_rank(status, mode, recency),
    )
}

/// A tab's label as the tab bar shows it.
pub(crate) fn tab_label(tab: &ClientShellTab) -> String {
    if tab.zoomed {
        format!("{} Z", tab.label)
    } else {
        tab.label.clone()
    }
}

/// Whether the global menu should show its attention dot.
pub(crate) fn update_attention(snapshot: &ClientShellSnapshot) -> bool {
    snapshot.update_available.is_some() || snapshot.integration_updates_available
}

/// How a semantic notification reaches the person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationToast {
    None,
    /// The client's own toast.
    InApp,
    /// The host terminal's notification escape.
    Terminal,
    /// The desktop notification service.
    System,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotificationDelivery {
    pub sound: bool,
    pub toast: NotificationToast,
}

/// Decides delivery for one notification. `target_active` is whether its
/// pane is what the person is looking at; `window_focused` is the host
/// window's focus, `None` when unknown.
pub fn notification_delivery(
    kind: SemanticNotificationKind,
    has_sound: bool,
    delivery: ToastDelivery,
    target_active: bool,
    window_focused: Option<bool>,
) -> NotificationDelivery {
    let suppress_external = target_active && window_focused != Some(false);
    let sound = has_sound && !(kind == SemanticNotificationKind::Finished && suppress_external);
    let toast = match delivery {
        ToastDelivery::Off => NotificationToast::None,
        ToastDelivery::Herdr if !target_active => NotificationToast::InApp,
        ToastDelivery::Herdr => NotificationToast::None,
        ToastDelivery::Terminal if !suppress_external => NotificationToast::Terminal,
        ToastDelivery::System if !suppress_external => NotificationToast::System,
        ToastDelivery::Terminal | ToastDelivery::System => NotificationToast::None,
    };
    NotificationDelivery { sound, toast }
}

/// What the sidebar model is built from: per-person settings that live
/// outside snapshots.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SidebarPrefs {
    pub spaces: SpacesSidebarConfig,
    pub agents: AgentsSidebarConfig,
    pub sort: AgentPanelSortConfig,
    /// Collapsed worktree groups per endpoint id.
    pub collapsed_groups: BTreeMap<String, HashSet<String>>,
    /// The primary leader (prefix) key label, e.g. `ctrl+b`.
    pub leader: String,
    /// `[ui.toast] delivery`, the input to [`notification_delivery`].
    pub toast_delivery: crate::config::ToastDelivery,
    /// `[ui.sound] enabled`.
    pub sound_enabled: bool,
}

impl SidebarPrefs {
    /// Reads the person's sidebar settings and leader key from the oasis
    /// config, as the TUI does at start.
    pub fn load() -> Self {
        let config = crate::config::Config::load().config;
        let keybinds = config
            .live_keybinds_with_diagnostics()
            .map(|(keybinds, _)| keybinds)
            .unwrap_or_else(|_| crate::config::LiveKeybindConfig {
                prefix: config.prefix_keys(),
                keybinds: config.keybinds(),
            });
        Self {
            spaces: config.ui.sidebar.spaces.clone(),
            agents: config.ui.sidebar.agents.clone(),
            sort: config.ui.agent_panel_sort,
            collapsed_groups: BTreeMap::new(),
            leader: keybinds.primary_prefix_label(),
            toast_delivery: config.ui.toast.delivery,
            sound_enabled: config.ui.sound.enabled,
        }
    }
}

/// One endpoint's input to the model.
#[derive(Clone, Copy, Debug)]
pub struct SidebarEndpoint<'a> {
    /// The endpoint's stable id, as `embed::EndpointId::as_str`.
    pub id: &'a str,
    pub label: &'a str,
    pub local: bool,
    pub online: bool,
    pub snapshot: Option<&'a ClientShellSnapshot>,
}

/// A worktree group toggle on a parent repository row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupToggle {
    pub key: String,
    pub collapsed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceEntry {
    pub workspace_id: String,
    pub number: usize,
    pub focused: bool,
    pub indented: bool,
    pub last_child: bool,
    pub group: Option<GroupToggle>,
    /// The displayed status, rolled up when the group is collapsed.
    pub status: AgentStatus,
    pub rows: Vec<TokenRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabEntry {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: usize,
    /// The tab bar label, with ` Z` when zoomed.
    pub label: String,
    pub focused: bool,
    pub zoomed: bool,
    pub status: AgentStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineEntry {
    pub id: String,
    pub label: String,
    pub local: bool,
    pub online: bool,
    pub focused_workspace_id: Option<String>,
    pub spaces: Vec<SpaceEntry>,
    /// The tabs of the focused workspace, in tab bar order.
    pub tabs: Vec<TabEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentEntry {
    pub endpoint: String,
    pub machine_label: String,
    pub pane_id: String,
    pub workspace_id: String,
    pub status: AgentStatus,
    /// A goal or loop driving the pane, which colors its state icon.
    pub mode: Option<crate::ui::AgentMode>,
    /// Which tool runs in the pane, from the snapshot's canonical `agent`
    /// label (not the presentation-only `display_agent`). `None` when the
    /// label names no agent Oasis knows.
    pub agent: Option<crate::detect::Agent>,
    pub focused: bool,
    /// The endpoint is offline; its rows are the last ones seen.
    pub stale: bool,
    pub rows: Vec<TokenRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct FooterEntry {
    pub leader: String,
    pub update_attention: bool,
}

/// Everything a sidebar shows, rebuilt from snapshots when they change.
#[derive(Clone, Debug, Default)]
pub struct SidebarModel {
    revision: u64,
    pub machines: Vec<MachineEntry>,
    /// Agents across every machine, in sidebar order.
    pub agents: Vec<AgentEntry>,
    pub footer: FooterEntry,
}

impl PartialEq for SidebarModel {
    fn eq(&self, other: &Self) -> bool {
        self.machines == other.machines
            && self.agents == other.agents
            && self.footer == other.footer
    }
}

impl SidebarModel {
    pub fn build(endpoints: &[SidebarEndpoint<'_>], prefs: &SidebarPrefs) -> Self {
        let empty = HashSet::new();
        let machines = endpoints
            .iter()
            .map(|endpoint| {
                let collapsed = prefs.collapsed_groups.get(endpoint.id).unwrap_or(&empty);
                machine_entry(endpoint, collapsed, &prefs.spaces)
            })
            .collect();
        let multi = endpoints.len() > 1;
        let mut agents = endpoints
            .iter()
            .filter_map(|endpoint| endpoint.snapshot.map(|snapshot| (endpoint, snapshot)))
            .flat_map(|(endpoint, snapshot)| {
                let machine = (multi && !endpoint.local).then_some(endpoint.label);
                agent_rows(snapshot, &prefs.agents, prefs.sort, machine)
                    .into_iter()
                    .filter_map(move |row| {
                        let agent = snapshot
                            .agents
                            .iter()
                            .find(|agent| agent.pane_id == row.pane_id)?;
                        Some((
                            (
                                agent.state_change_seq,
                                idle_background_load(row.status, row.mode, agent),
                            ),
                            AgentEntry {
                                endpoint: endpoint.id.to_owned(),
                                machine_label: endpoint.label.to_owned(),
                                pane_id: row.pane_id,
                                workspace_id: agent.workspace_id.clone(),
                                status: row.status,
                                mode: row.mode,
                                agent: agent
                                    .agent
                                    .as_deref()
                                    .and_then(crate::detect::parse_agent_label),
                                focused: row.focused,
                                stale: !endpoint.online,
                                rows: row.rows,
                            },
                        ))
                    })
            })
            .collect::<Vec<_>>();
        if prefs.sort == AgentPanelSortConfig::Priority {
            agents.sort_by_key(|((recency, load), entry)| {
                combined_priority_key(entry.stale, entry.status, entry.mode, *load, *recency)
            });
        }
        let footer = FooterEntry {
            leader: prefs.leader.clone(),
            update_attention: endpoints
                .iter()
                .filter_map(|endpoint| endpoint.snapshot)
                .any(update_attention),
        };
        Self {
            revision: 0,
            machines,
            agents: agents.into_iter().map(|(_, entry)| entry).collect(),
            footer,
        }
    }

    /// Rebuilds from `endpoints`; returns whether anything a sidebar shows
    /// changed, and bumps [`Self::revision`] when it did.
    pub fn update(&mut self, endpoints: &[SidebarEndpoint<'_>], prefs: &SidebarPrefs) -> bool {
        let next = Self::build(endpoints, prefs);
        if next == *self && self.revision != 0 {
            return false;
        }
        let revision = self.revision.wrapping_add(1).max(1);
        *self = next;
        self.revision = revision;
        true
    }

    /// Increases each time the content changes; 0 before the first build.
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

fn machine_entry(
    endpoint: &SidebarEndpoint<'_>,
    collapsed: &HashSet<String>,
    spaces_config: &SpacesSidebarConfig,
) -> MachineEntry {
    let Some(snapshot) = endpoint.snapshot else {
        return MachineEntry {
            id: endpoint.id.to_owned(),
            label: endpoint.label.to_owned(),
            local: endpoint.local,
            online: endpoint.online,
            focused_workspace_id: None,
            spaces: Vec::new(),
            tabs: Vec::new(),
        };
    };
    let spaces = workspace_entries(snapshot, collapsed)
        .into_iter()
        .filter_map(|entry| {
            let workspace = snapshot.workspaces.get(entry.index)?;
            let status = displayed_workspace_status(snapshot, workspace, collapsed);
            Some(SpaceEntry {
                workspace_id: workspace.workspace_id.clone(),
                number: workspace.number,
                focused: workspace.focused,
                indented: entry.indented,
                last_child: entry.last_child,
                group: parent_group_key(snapshot, entry.index).map(|key| GroupToggle {
                    collapsed: collapsed.contains(&key),
                    key,
                }),
                status,
                rows: workspace_rows(workspace, status, entry.indented, spaces_config),
            })
        })
        .collect();
    let focused_workspace_id = snapshot.focused_workspace_id.clone();
    let tabs = snapshot
        .tabs
        .iter()
        .filter(|tab| Some(&tab.workspace_id) == focused_workspace_id.as_ref())
        .map(|tab| TabEntry {
            tab_id: tab.tab_id.clone(),
            workspace_id: tab.workspace_id.clone(),
            number: tab.number,
            label: tab_label(tab),
            focused: tab.focused,
            zoomed: tab.zoomed,
            status: tab.agent_status,
        })
        .collect();
    MachineEntry {
        id: endpoint.id.to_owned(),
        label: endpoint.label.to_owned(),
        local: endpoint.local,
        online: endpoint.online,
        focused_workspace_id,
        spaces,
        tabs,
    }
}

/// Something the person asked to close that the server has not confirmed yet.
/// Front ends hide it at once so a slow server does not make the close feel
/// slow; see [`hide_closed_target`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseTarget {
    Tab(String),
    Pane(String),
    /// A workspace, or a worktree group closed together.
    Workspaces(Vec<String>),
}

impl CloseTarget {
    /// Whether `snapshot` still contains anything this close removes.
    pub fn present_in(&self, snapshot: &ClientShellSnapshot) -> bool {
        match self {
            Self::Tab(tab_id) => snapshot.tabs.iter().any(|tab| &tab.tab_id == tab_id),
            Self::Pane(pane_id) => snapshot.panes.iter().any(|pane| &pane.pane_id == pane_id),
            Self::Workspaces(ids) => snapshot
                .workspaces
                .iter()
                .any(|workspace| ids.contains(&workspace.workspace_id)),
        }
    }
}

/// Removes `target` from `snapshot` the way the server will, moving focus to
/// the neighbor it picks: the previous tab (or the next one at the start), a
/// remaining pane in the tab, or the workspace that takes the closed one's
/// place. Closing a tab's last pane closes the tab, and a workspace's last tab
/// closes the workspace, as on the server.
pub fn hide_closed_target(snapshot: &mut ClientShellSnapshot, target: &CloseTarget) {
    match target {
        CloseTarget::Pane(pane_id) => {
            let Some(pane) = snapshot.panes.iter().find(|pane| &pane.pane_id == pane_id) else {
                return;
            };
            let tab_id = pane.tab_id.clone();
            let last_in_tab = !snapshot
                .panes
                .iter()
                .any(|other| other.tab_id == tab_id && &other.pane_id != pane_id);
            if last_in_tab {
                return hide_closed_target(snapshot, &CloseTarget::Tab(tab_id));
            }
            snapshot.panes.retain(|pane| &pane.pane_id != pane_id);
            snapshot.agents.retain(|agent| &agent.pane_id != pane_id);
            snapshot.agent_order.retain(|id| id != pane_id);
            if snapshot.focused_pane_id.as_deref() == Some(pane_id.as_str()) {
                let next = snapshot
                    .panes
                    .iter()
                    .find(|pane| pane.tab_id == tab_id)
                    .map(|pane| pane.pane_id.clone());
                set_focused_pane(snapshot, next);
            }
        }
        CloseTarget::Tab(tab_id) => {
            let Some(tab) = snapshot.tabs.iter().find(|tab| &tab.tab_id == tab_id) else {
                return;
            };
            let workspace_id = tab.workspace_id.clone();
            let siblings = snapshot
                .tabs
                .iter()
                .filter(|tab| tab.workspace_id == workspace_id)
                .map(|tab| tab.tab_id.clone())
                .collect::<Vec<_>>();
            if siblings.len() <= 1 {
                return hide_closed_target(snapshot, &CloseTarget::Workspaces(vec![workspace_id]));
            }
            let closed = siblings.iter().position(|id| id == tab_id).unwrap_or(0);
            let active_tab_id = snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.workspace_id == workspace_id)
                .map(|workspace| workspace.active_tab_id.clone());
            let mut active = active_tab_id
                .and_then(|active| siblings.iter().position(|id| *id == active))
                .unwrap_or(0);
            // Workspace::close_tab
            let remaining = siblings.len() - 1;
            if active >= remaining {
                active = remaining - 1;
            } else if closed <= active && active > 0 {
                active -= 1;
            }
            let remaining_ids = siblings
                .iter()
                .filter(|id| *id != tab_id)
                .collect::<Vec<_>>();
            let next_active = remaining_ids[active.min(remaining_ids.len() - 1)].clone();
            remove_tab_contents(snapshot, tab_id);
            renumber(
                snapshot
                    .tabs
                    .iter_mut()
                    .filter(|tab| tab.workspace_id == workspace_id)
                    .map(|tab| &mut tab.number),
            );
            if let Some(workspace) = snapshot
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.workspace_id == workspace_id)
            {
                workspace.active_tab_id = next_active.clone();
            }
            if snapshot.focused_workspace_id.as_deref() == Some(workspace_id.as_str()) {
                focus_tab(snapshot, Some(next_active));
            }
        }
        CloseTarget::Workspaces(ids) => {
            let first_closed = snapshot
                .workspaces
                .iter()
                .position(|workspace| ids.contains(&workspace.workspace_id));
            let Some(first_closed) = first_closed else {
                return;
            };
            let focused_closed = snapshot
                .focused_workspace_id
                .as_ref()
                .is_some_and(|focused| ids.contains(focused));
            let tab_ids = snapshot
                .tabs
                .iter()
                .filter(|tab| ids.contains(&tab.workspace_id))
                .map(|tab| tab.tab_id.clone())
                .collect::<Vec<_>>();
            for tab_id in &tab_ids {
                remove_tab_contents(snapshot, tab_id);
            }
            snapshot
                .workspaces
                .retain(|workspace| !ids.contains(&workspace.workspace_id));
            renumber(
                snapshot
                    .workspaces
                    .iter_mut()
                    .map(|workspace| &mut workspace.number),
            );
            if focused_closed {
                // AppState::close_workspaces keeps the closed index, clamped.
                let next = snapshot
                    .workspaces
                    .get(first_closed.min(snapshot.workspaces.len().saturating_sub(1)))
                    .map(|workspace| workspace.workspace_id.clone());
                for workspace in &mut snapshot.workspaces {
                    workspace.focused = Some(&workspace.workspace_id) == next.as_ref();
                }
                let active_tab = next.as_ref().and_then(|next| {
                    snapshot
                        .workspaces
                        .iter()
                        .find(|workspace| &workspace.workspace_id == next)
                        .map(|workspace| workspace.active_tab_id.clone())
                });
                snapshot.focused_workspace_id = next;
                focus_tab(snapshot, active_tab);
            }
        }
    }
}

/// Closes hidden before their server confirms them, per endpoint `K` and
/// request `R`. Both the TUI and [`crate::client::core::ClientCore`] keep one:
/// the target is hidden from every snapshot until a snapshot without it, or a
/// success answer followed by the next snapshot, settles it. A failure puts it
/// back, and a snapshot from a new server boot drops it.
#[derive(Debug)]
pub struct PendingCloses<K, R> {
    entries: Vec<PendingCloseEntry<K, R>>,
}

#[derive(Debug)]
struct PendingCloseEntry<K, R> {
    request: R,
    endpoint: K,
    boot_id: String,
    target: CloseTarget,
    /// The server answered success; the next snapshot is the truth.
    confirmed: bool,
}

/// What a server answer did to a pending close.
#[derive(Debug, PartialEq, Eq)]
pub enum CloseSettle<K> {
    /// No close waits on that request.
    Unknown,
    /// Succeeded, but the last snapshot still shows the target: keep hiding it
    /// until the next one.
    Confirmed,
    /// No longer pending; rebuild the endpoint's shown snapshot.
    Settled { endpoint: K, failed: bool },
}

/// What [`PendingCloses::filter_incoming`] did to a snapshot.
#[derive(Debug)]
pub enum IncomingCloses {
    /// Nothing was pending for the endpoint.
    Untracked,
    /// The snapshot settled every pending close for the endpoint.
    AllSettled,
    /// Some closes are still pending and were hidden; this is the snapshot as
    /// the server sent it.
    StillHidden(Box<ClientShellSnapshot>),
}

impl<K, R> Default for PendingCloses<K, R> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

impl<K: PartialEq + Clone, R: PartialEq> PendingCloses<K, R> {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn begin(&mut self, request: R, endpoint: K, boot_id: String, target: CloseTarget) {
        self.entries.push(PendingCloseEntry {
            request,
            endpoint,
            boot_id,
            target,
            confirmed: false,
        });
    }

    pub fn tracks(&self, endpoint: &K) -> bool {
        self.entries.iter().any(|entry| &entry.endpoint == endpoint)
    }

    pub fn contains_request(&self, request: &R) -> bool {
        self.entries.iter().any(|entry| &entry.request == request)
    }

    /// Hides every close pending on `endpoint`; returns whether any was.
    pub fn apply(&self, endpoint: &K, snapshot: &mut ClientShellSnapshot) -> bool {
        let mut any = false;
        for entry in self
            .entries
            .iter()
            .filter(|entry| &entry.endpoint == endpoint)
        {
            hide_closed_target(snapshot, &entry.target);
            any = true;
        }
        any
    }

    /// Applies the server's answer to `request`. `raw` gives the endpoint's
    /// last snapshot as the server sent it.
    pub fn settle<'a>(
        &mut self,
        request: &R,
        succeeded: bool,
        raw: impl FnOnce(&K) -> Option<&'a ClientShellSnapshot>,
    ) -> CloseSettle<K> {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| &entry.request == request)
        else {
            return CloseSettle::Unknown;
        };
        if succeeded
            && raw(&self.entries[index].endpoint)
                .is_some_and(|snapshot| self.entries[index].target.present_in(snapshot))
        {
            self.entries[index].confirmed = true;
            return CloseSettle::Confirmed;
        }
        let entry = self.entries.remove(index);
        CloseSettle::Settled {
            endpoint: entry.endpoint,
            failed: !succeeded,
        }
    }

    /// Settles the closes `snapshot` shows are done (or that belong to an
    /// earlier boot) and hides the rest in it.
    pub fn filter_incoming(
        &mut self,
        endpoint: &K,
        snapshot: &mut ClientShellSnapshot,
    ) -> IncomingCloses {
        if !self.tracks(endpoint) {
            return IncomingCloses::Untracked;
        }
        self.entries.retain(|entry| {
            &entry.endpoint != endpoint
                || (entry.boot_id == snapshot.boot_id
                    && !entry.confirmed
                    && entry.target.present_in(snapshot))
        });
        if !self.tracks(endpoint) {
            return IncomingCloses::AllSettled;
        }
        let raw = Box::new(snapshot.clone());
        self.apply(endpoint, snapshot);
        IncomingCloses::StillHidden(raw)
    }

    /// Drops one pending close, e.g. when its request timed out. Returns its
    /// endpoint.
    pub fn remove_request(&mut self, request: &R) -> Option<K> {
        let index = self
            .entries
            .iter()
            .position(|entry| &entry.request == request)?;
        Some(self.entries.remove(index).endpoint)
    }

    /// Drops every close pending on `endpoint`; returns whether any was.
    pub fn abandon(&mut self, endpoint: &K) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| &entry.endpoint != endpoint);
        self.entries.len() != before
    }
}

fn remove_tab_contents(snapshot: &mut ClientShellSnapshot, tab_id: &str) {
    let pane_ids = snapshot
        .panes
        .iter()
        .filter(|pane| pane.tab_id == tab_id)
        .map(|pane| pane.pane_id.clone())
        .collect::<HashSet<_>>();
    snapshot.tabs.retain(|tab| tab.tab_id != tab_id);
    snapshot.panes.retain(|pane| pane.tab_id != tab_id);
    snapshot
        .agents
        .retain(|agent| !pane_ids.contains(&agent.pane_id));
    snapshot.agent_order.retain(|id| !pane_ids.contains(id));
}

/// Keeps numbers 1..n when they were 1..n, as the server renumbers on close.
fn renumber<'a>(numbers: impl Iterator<Item = &'a mut usize>) {
    let mut numbers = numbers.collect::<Vec<_>>();
    numbers.sort_by_key(|number| **number);
    for (index, number) in numbers.into_iter().enumerate() {
        *number = index + 1;
    }
}

fn focus_tab(snapshot: &mut ClientShellSnapshot, tab_id: Option<String>) {
    for tab in &mut snapshot.tabs {
        tab.focused = Some(&tab.tab_id) == tab_id.as_ref();
    }
    let pane = tab_id.as_ref().and_then(|tab_id| {
        snapshot
            .panes
            .iter()
            .find(|pane| &pane.tab_id == tab_id)
            .map(|pane| pane.pane_id.clone())
    });
    snapshot.focused_tab_id = tab_id;
    set_focused_pane(snapshot, pane);
}

fn set_focused_pane(snapshot: &mut ClientShellSnapshot, pane_id: Option<String>) {
    for pane in &mut snapshot.panes {
        pane.focused = Some(&pane.pane_id) == pane_id.as_ref();
    }
    for agent in &mut snapshot.agents {
        agent.focused = Some(&agent.pane_id) == pane_id.as_ref();
    }
    snapshot.focused_pane_id = pane_id;
}

#[cfg(test)]
#[path = "model_tests.rs"]
pub(crate) mod tests;
