use super::*;
use crate::config::AgentSidebarToken;
use crate::protocol::{ClientShellAgent, ClientShellPane, ClientShellWorktree};
use crate::ui::ResolvedTokenKind;

pub(crate) fn workspace(id: &str, number: usize, status: AgentStatus) -> ClientShellWorkspace {
    ClientShellWorkspace {
        workspace_id: id.into(),
        active_tab_id: format!("{id}_tab"),
        new_workspace_cwd: "/repo".into(),
        number,
        label: id.into(),
        custom_label: false,
        branch: None,
        git_ahead_behind: None,
        tokens: Vec::new(),
        worktree: None,
        focused: false,
        agent_status: status,
    }
}

fn worktree(key: &str, linked: bool) -> Option<ClientShellWorktree> {
    Some(ClientShellWorktree {
        key: key.into(),
        label: key.into(),
        is_linked_worktree: linked,
    })
}

fn agent(pane_id: &str, workspace_id: &str, status: AgentStatus, seq: u64) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: workspace_id.into(),
        tab_id: format!("{workspace_id}_tab"),
        name: None,
        display_agent: None,
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: status,
        state_change_seq: seq,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
    }
}

pub(crate) fn snapshot(
    workspaces: Vec<ClientShellWorkspace>,
    agents: Vec<ClientShellAgent>,
) -> ClientShellSnapshot {
    let tabs = workspaces
        .iter()
        .map(|workspace| ClientShellTab {
            tab_id: workspace.active_tab_id.clone(),
            workspace_id: workspace.workspace_id.clone(),
            number: 1,
            label: "1".into(),
            custom_label: false,
            zoomed: false,
            focused: workspace.focused,
            agent_status: workspace.agent_status,
        })
        .collect();
    let panes = agents
        .iter()
        .map(|agent| ClientShellPane {
            pane_id: agent.pane_id.clone(),
            workspace_id: agent.workspace_id.clone(),
            tab_id: agent.tab_id.clone(),
            label: None,
            cwd: None,
            foreground_cwd: None,
            focused: false,
            right_click_passthrough: false,
        })
        .collect();
    ClientShellSnapshot {
        boot_id: "boot".into(),
        revision: 1,
        config_diagnostic: None,
        product_announcement: None,
        update_available: None,
        update_install_command: "oasis update".into(),
        server_keybindings_toml: None,
        latest_release_notes_available: false,
        integration_updates_available: false,
        worktree_directory: "/worktrees".into(),
        release_notes: None,
        focused_workspace_id: workspaces
            .iter()
            .find(|workspace| workspace.focused)
            .map(|workspace| workspace.workspace_id.clone()),
        focused_tab_id: None,
        focused_pane_id: None,
        tab_bar_right: Vec::new(),
        tab_bar_right_separator: " ".into(),
        agent_view_label: None,
        agent_order: Vec::new(),
        workspaces,
        tabs,
        panes,
        agents,
        commands: Vec::new(),
    }
}

fn endpoint<'a>(id: &'a str, snapshot: &'a ClientShellSnapshot) -> SidebarEndpoint<'a> {
    SidebarEndpoint {
        id,
        label: id,
        local: id == "local",
        online: true,
        snapshot: Some(snapshot),
    }
}

fn row_text(row: &TokenRow) -> Vec<String> {
    row.iter()
        .filter_map(|token| match &token.kind {
            ResolvedTokenKind::Machine(text)
            | ResolvedTokenKind::Workspace(text)
            | ResolvedTokenKind::Agent(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn worktree_children_nest_and_a_collapsed_parent_rolls_up_their_status() {
    let mut parent = workspace("repo", 1, AgentStatus::Idle);
    parent.worktree = worktree("repo", false);
    let mut child = workspace("feature", 2, AgentStatus::Blocked);
    child.worktree = worktree("repo", true);
    child.branch = Some("worktree/feature".into());
    let mut other = workspace("notes", 3, AgentStatus::Working);
    other.focused = true;
    let snapshot = snapshot(vec![parent, other, child], Vec::new());

    let mut prefs = SidebarPrefs::default();
    let model = SidebarModel::build(&[endpoint("local", &snapshot)], &prefs);
    let spaces = &model.machines[0].spaces;
    assert_eq!(
        spaces
            .iter()
            .map(|space| (
                space.workspace_id.as_str(),
                space.indented,
                space.last_child
            ))
            .collect::<Vec<_>>(),
        [
            ("repo", false, false),
            ("feature", true, true),
            ("notes", false, false)
        ]
    );
    assert_eq!(
        spaces[0].group,
        Some(GroupToggle {
            key: "repo".into(),
            collapsed: false
        })
    );
    assert_eq!(spaces[0].status, AgentStatus::Idle);
    assert_eq!(
        model.machines[0].focused_workspace_id.as_deref(),
        Some("notes")
    );

    prefs
        .collapsed_groups
        .insert("local".into(), HashSet::from(["repo".to_owned()]));
    let collapsed = SidebarModel::build(&[endpoint("local", &snapshot)], &prefs);
    let spaces = &collapsed.machines[0].spaces;
    assert_eq!(
        spaces
            .iter()
            .map(|space| space.workspace_id.as_str())
            .collect::<Vec<_>>(),
        ["repo", "notes"],
        "an unfocused child of a collapsed group is hidden"
    );
    assert_eq!(
        spaces[0].status,
        AgentStatus::Blocked,
        "the parent shows its child's status"
    );
    assert!(spaces[0]
        .group
        .as_ref()
        .is_some_and(|group| group.collapsed));
}

#[test]
fn combined_agents_span_machines_and_sort_by_urgency_with_live_machines_first() {
    let local = snapshot(
        vec![workspace("ws_l", 1, AgentStatus::Idle)],
        vec![
            agent("l_idle", "ws_l", AgentStatus::Idle, 5),
            agent("l_work", "ws_l", AgentStatus::Working, 2),
        ],
    );
    let remote = snapshot(
        vec![workspace("ws_r", 1, AgentStatus::Blocked)],
        vec![agent("r_block", "ws_r", AgentStatus::Blocked, 1)],
    );
    let offline = snapshot(
        vec![workspace("ws_o", 1, AgentStatus::Blocked)],
        vec![agent("o_block", "ws_o", AgentStatus::Blocked, 9)],
    );
    let mut down = endpoint("ssh:down", &offline);
    down.online = false;
    let endpoints = [
        endpoint("local", &local),
        endpoint("ssh:box", &remote),
        down,
    ];

    let mut prefs = SidebarPrefs {
        agents: AgentsSidebarConfig {
            rows: vec![vec![
                AgentSidebarToken::Machine,
                AgentSidebarToken::Workspace,
            ]],
            ..AgentsSidebarConfig::default()
        },
        ..SidebarPrefs::default()
    };
    let spaces_order = SidebarModel::build(&endpoints, &prefs);
    assert_eq!(
        spaces_order
            .agents
            .iter()
            .map(|agent| agent.pane_id.as_str())
            .collect::<Vec<_>>(),
        ["l_idle", "l_work", "r_block", "o_block"],
        "spaces order keeps machine then snapshot order"
    );
    let remote_row = &spaces_order.agents[2];
    assert_eq!(remote_row.endpoint, "ssh:box");
    assert_eq!(row_text(&remote_row.rows[0]), ["ssh:box", "ws_r"]);
    assert_eq!(
        row_text(&spaces_order.agents[0].rows[0]),
        ["ws_l"],
        "local agents carry no machine name"
    );
    assert!(spaces_order.agents[3].stale);

    prefs.sort = AgentPanelSortConfig::Priority;
    let priority = SidebarModel::build(&endpoints, &prefs);
    assert_eq!(
        priority
            .agents
            .iter()
            .map(|agent| agent.pane_id.as_str())
            .collect::<Vec<_>>(),
        ["r_block", "l_idle", "l_work", "o_block"],
        "offline machines sort last even when blocked"
    );
}

/// Priority order: ended turns (done or a goal just met), needing you, idle,
/// busy (working or waiting on subagents), goals, then loops. A goal or loop
/// pane keeps its place whether it works or not, unless it needs you.
#[test]
fn priority_ranks_done_needing_you_idle_busy_goals_then_loops() {
    let with = |mut agent: ClientShellAgent, key: &str| {
        agent.tokens = vec![(key.to_owned(), "1".to_owned())];
        agent
    };
    let snap = snapshot(
        vec![workspace("ws", 1, AgentStatus::Idle)],
        vec![
            with(agent("loop_done", "ws", AgentStatus::Done, 9), "oasis:loop"),
            with(
                agent("loop_work", "ws", AgentStatus::Working, 10),
                "oasis:loop",
            ),
            with(agent("goal_idle", "ws", AgentStatus::Idle, 8), "oasis:goal"),
            agent("plain_done", "ws", AgentStatus::Done, 1),
            agent("plain_work", "ws", AgentStatus::Working, 2),
            with(
                agent("goal_ask", "ws", AgentStatus::Blocked, 3),
                "oasis:goal",
            ),
            with(
                agent("goal_met", "ws", AgentStatus::Done, 0),
                "oasis:goal_achieved",
            ),
            with(agent("waiting", "ws", AgentStatus::Idle, 6), "oasis:agents"),
            agent("plain_idle", "ws", AgentStatus::Idle, 7),
        ],
    );
    assert_eq!(
        ordered_agent_pane_ids(&snap, AgentPanelSortConfig::Priority),
        [
            "plain_done",
            "goal_met",
            "goal_ask",
            "plain_idle",
            "waiting",
            "plain_work",
            "goal_idle",
            "loop_work",
            "loop_done",
        ]
    );
}

/// Idle panes still running background work go below plain idle ones, more
/// work lower and, at the same total, more monitors lower. Other tiers keep
/// recency order.
#[test]
fn priority_sinks_idle_panes_by_their_background_work() {
    let with = |mut agent: ClientShellAgent, tokens: &[(&str, &str)]| {
        agent.tokens = tokens
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        agent
    };
    let snap = snapshot(
        vec![workspace("ws", 1, AgentStatus::Idle)],
        vec![
            with(
                agent("mixed", "ws", AgentStatus::Idle, 9),
                &[("oasis:shells", "1"), ("oasis:monitors", "1")],
            ),
            with(
                agent("two_shells", "ws", AgentStatus::Idle, 8),
                &[("oasis:shells", "2")],
            ),
            with(
                agent("one_task", "ws", AgentStatus::Idle, 7),
                &[("oasis:tasks", "1")],
            ),
            agent("plain", "ws", AgentStatus::Idle, 1),
            with(
                agent("work_shells", "ws", AgentStatus::Working, 6),
                &[("oasis:shells", "3")],
            ),
            agent("work", "ws", AgentStatus::Working, 5),
        ],
    );
    let expected = [
        "plain",
        "one_task",
        "two_shells",
        "mixed",
        "work_shells",
        "work",
    ];
    assert_eq!(
        ordered_agent_pane_ids(&snap, AgentPanelSortConfig::Priority),
        expected
    );
    let prefs = SidebarPrefs {
        sort: AgentPanelSortConfig::Priority,
        ..SidebarPrefs::default()
    };
    let combined = SidebarModel::build(&[endpoint("local", &snap)], &prefs);
    assert_eq!(
        combined
            .agents
            .iter()
            .map(|agent| agent.pane_id.as_str())
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn update_reports_changes_only_when_shown_content_changes() {
    let mut snap = snapshot(
        vec![workspace("ws", 1, AgentStatus::Idle)],
        vec![agent("pane", "ws", AgentStatus::Idle, 1)],
    );
    let prefs = SidebarPrefs {
        leader: "ctrl+b".into(),
        ..SidebarPrefs::default()
    };
    let mut model = SidebarModel::default();
    assert_eq!(model.revision(), 0);
    assert!(model.update(&[endpoint("local", &snap)], &prefs));
    assert_eq!(model.revision(), 1);
    assert_eq!(model.footer.leader, "ctrl+b");

    snap.revision += 1;
    snap.focused_pane_id = Some("pane".into());
    assert!(
        !model.update(&[endpoint("local", &snap)], &prefs),
        "a snapshot change the sidebar does not show is not a change"
    );
    assert_eq!(model.revision(), 1);

    snap.agents[0].agent_status = AgentStatus::Working;
    assert!(model.update(&[endpoint("local", &snap)], &prefs));
    assert_eq!(model.revision(), 2);
    assert_eq!(model.agents[0].status, AgentStatus::Working);

    snap.update_available = Some("1.0.0".into());
    assert!(model.update(&[endpoint("local", &snap)], &prefs));
    assert!(model.footer.update_attention);
}

#[test]
fn tabs_cover_the_focused_workspace_and_mark_zoom() {
    let mut focused = workspace("ws_a", 1, AgentStatus::Idle);
    focused.focused = true;
    let mut snap = snapshot(
        vec![focused, workspace("ws_b", 2, AgentStatus::Idle)],
        Vec::new(),
    );
    snap.tabs[0].zoomed = true;
    snap.tabs[0].label = "edit".into();
    let model = SidebarModel::build(&[endpoint("local", &snap)], &SidebarPrefs::default());
    let tabs = &model.machines[0].tabs;
    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].label, "edit Z");
    assert!(tabs[0].zoomed);
}

#[test]
fn a_machine_without_a_snapshot_has_no_rows() {
    let model = SidebarModel::build(
        &[SidebarEndpoint {
            id: "ssh:box",
            label: "box",
            local: false,
            online: false,
            snapshot: None,
        }],
        &SidebarPrefs::default(),
    );
    assert_eq!(model.machines[0].label, "box");
    assert!(model.machines[0].spaces.is_empty());
    assert!(model.agents.is_empty());
}

#[test]
fn notification_delivery_matches_the_toast_settings() {
    use SemanticNotificationKind::{Finished, NeedsAttention};
    let decide = notification_delivery;
    // Looking at the pane in a focused window: no in-app toast, finished
    // sounds stay quiet, other sounds still play.
    assert_eq!(
        decide(Finished, true, ToastDelivery::Herdr, true, Some(true)),
        NotificationDelivery {
            sound: false,
            toast: NotificationToast::None
        }
    );
    assert!(decide(NeedsAttention, true, ToastDelivery::Herdr, true, Some(true)).sound);
    // Elsewhere: in-app toast.
    assert_eq!(
        decide(Finished, true, ToastDelivery::Herdr, false, Some(true)).toast,
        NotificationToast::InApp
    );
    // External toasts stay quiet only while the person looks at the pane in a
    // focused window; an unfocused window still gets them.
    assert_eq!(
        decide(Finished, false, ToastDelivery::System, true, Some(true)).toast,
        NotificationToast::None
    );
    assert_eq!(
        decide(Finished, false, ToastDelivery::System, true, Some(false)).toast,
        NotificationToast::System
    );
    assert_eq!(
        decide(Finished, false, ToastDelivery::Terminal, false, None).toast,
        NotificationToast::Terminal
    );
    assert_eq!(
        decide(Finished, true, ToastDelivery::Off, false, None),
        NotificationDelivery {
            sound: true,
            toast: NotificationToast::None
        }
    );
}

#[test]
fn sidebar_prefs_load_reads_toast_delivery_and_sound() {
    let dir = std::env::temp_dir().join(format!("oasis-prefs-{}", std::process::id()));
    let config_dir = dir.join(crate::config::app_dir_name());
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        "onboarding = false\n[ui.toast]\ndelivery = \"system\"\n[ui.sound]\nenabled = false\n",
    )
    .unwrap();
    std::env::set_var("XDG_CONFIG_HOME", &dir);
    let prefs = SidebarPrefs::load();
    assert_eq!(prefs.toast_delivery, ToastDelivery::System);
    assert!(!prefs.sound_enabled);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn hiding_the_first_active_tab_focuses_the_next_and_renumbers() {
    let mut first = workspace("ws_1", 1, AgentStatus::Idle);
    first.focused = true;
    first.active_tab_id = "tab_1".into();
    let mut snapshot = snapshot(
        vec![first],
        vec![agent("pane_1", "ws_1", AgentStatus::Idle, 1)],
    );
    snapshot.panes[0].tab_id = "tab_1".into();
    snapshot.focused_tab_id = Some("tab_1".into());
    snapshot.focused_pane_id = Some("pane_1".into());
    let mut second = snapshot.tabs[0].clone();
    second.tab_id = "tab_2".into();
    second.number = 2;
    second.focused = false;
    snapshot.tabs.push(second);
    let mut pane = snapshot.panes[0].clone();
    pane.pane_id = "pane_2".into();
    pane.tab_id = "tab_2".into();
    pane.focused = false;
    snapshot.panes.push(pane);
    hide_closed_target(&mut snapshot, &CloseTarget::Tab("tab_1".into()));
    assert_eq!(snapshot.focused_tab_id.as_deref(), Some("tab_2"));
    assert_eq!(snapshot.focused_pane_id.as_deref(), Some("pane_2"));
    assert_eq!(snapshot.tabs[0].number, 1);
    assert!(!CloseTarget::Tab("tab_1".into()).present_in(&snapshot));
}
