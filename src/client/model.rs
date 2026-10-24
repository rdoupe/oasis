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
            (
                std::cmp::Reverse(status_priority(agent.agent_status)),
                std::cmp::Reverse(agent.state_change_seq),
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
/// machines first, then most urgent status, then most recent change.
pub(crate) fn combined_priority_key(
    stale: bool,
    status: AgentStatus,
    recency: u64,
) -> (bool, std::cmp::Reverse<u8>, std::cmp::Reverse<u64>) {
    (
        stale,
        std::cmp::Reverse(status_priority(status)),
        std::cmp::Reverse(recency),
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
                            agent.state_change_seq,
                            AgentEntry {
                                endpoint: endpoint.id.to_owned(),
                                machine_label: endpoint.label.to_owned(),
                                pane_id: row.pane_id,
                                workspace_id: agent.workspace_id.clone(),
                                status: row.status,
                                focused: row.focused,
                                stale: !endpoint.online,
                                rows: row.rows,
                            },
                        ))
                    })
            })
            .collect::<Vec<_>>();
        if prefs.sort == AgentPanelSortConfig::Priority {
            agents.sort_by_key(|(recency, entry)| {
                combined_priority_key(entry.stale, entry.status, *recency)
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

#[cfg(test)]
#[path = "model_tests.rs"]
pub(crate) mod tests;
