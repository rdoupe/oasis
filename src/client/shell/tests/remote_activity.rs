use super::*;
use crate::client::endpoint::{
    ClientEndpointId, ClientEndpointStatus, ProfileId, SavedSshEndpoint,
};
use crate::client::shell::remote_activity::REMOTE_ACTIVITY_MIN_INTERVAL;

const FOOTER: &str = "⏺ Done.\n──────────\n❯ \n──────────\n  ⏵⏵ auto mode on · 1 shell, 1 monitor";

fn herdr_machine() -> (ClientShellState, ClientEndpointId) {
    let config: Config = toml::from_str(
        "onboarding = false\n[ui.sidebar.agents]\nrows = [[\"agent\"], [\"background\"]]\n",
    )
    .expect("config");
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&config));
    let profile = SavedSshEndpoint {
        id: ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "build-box".into(),
        target: "dev@build.example".into(),
        session: "default".into(),
        enabled: true,
    };
    let endpoint_id = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(&endpoint_id, ClientEndpointStatus::Online);
    // A herdr server: its welcome lacks the background-activity capability.
    state.set_endpoint_background_activity_supported(&endpoint_id, false);
    (state, endpoint_id)
}

fn claude(pane_id: &str, seq: u64, title: &str) -> ClientShellAgent {
    ClientShellAgent {
        pane_id: pane_id.into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: None,
        display_agent: None,
        agent: Some("claude".into()),
        title: None,
        terminal_title: Some(title.into()),
        terminal_title_stripped: Some(title.into()),
        agent_status: crate::api::schema::AgentStatus::Working,
        state_change_seq: seq,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: false,
    }
}

fn remote_snapshot(revision: u64, agents: Vec<ClientShellAgent>) -> Box<ClientShellSnapshot> {
    let mut remote = snapshot();
    remote.boot_id = "herdr-boot".into();
    remote.revision = revision;
    remote.agents = agents;
    Box::new(remote)
}

fn endpoint_agent_tokens(
    state: &ClientShellState,
    endpoint_id: &ClientEndpointId,
    pane_id: &str,
) -> Vec<(String, String)> {
    state
        .endpoints
        .iter()
        .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
        .and_then(|endpoint| endpoint.snapshot.as_deref())
        .and_then(|snapshot| {
            snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane_id)
        })
        .map(|agent| agent.tokens.clone())
        .unwrap_or_default()
}

#[test]
fn herdr_agent_change_requests_one_read_and_its_counts_reach_the_background_token() {
    let (mut state, endpoint_id) = herdr_machine();
    state.set_endpoint_snapshot(
        &endpoint_id,
        remote_snapshot(1, vec![claude("pane_1", 1, "✳ x")]),
    );
    let now = std::time::Instant::now();
    assert_eq!(state.remote_activity_due(now), Some(now));
    assert!(
        state.timer_delay(now).is_zero(),
        "a due read wakes the loop"
    );
    assert_eq!(
        state.take_remote_activity_reads(now),
        vec![(endpoint_id.clone(), "pane_1".to_owned())]
    );
    assert!(
        state.take_remote_activity_reads(now).is_empty(),
        "one read in flight"
    );

    assert!(state.receive_remote_agent_screen(&endpoint_id, "pane_1", Some(FOOTER)));
    assert_eq!(
        endpoint_agent_tokens(&state, &endpoint_id, "pane_1"),
        vec![
            ("oasis:monitors".to_owned(), "1".to_owned()),
            ("oasis:shells".to_owned(), "1".to_owned()),
        ]
    );
    let endpoint = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.endpoint_id == endpoint_id)
        .unwrap();
    let row = super::super::agent_sidebar::agent_row(
        endpoint.snapshot.as_deref().unwrap(),
        "pane_1",
        &state.config,
        Some("build-box"),
    )
    .expect("agent row");
    assert!(row.rows.iter().flatten().any(|token| matches!(
        &token.kind,
        crate::ui::ResolvedTokenKind::Activity(parts)
            if parts.iter().map(|part| part.text.as_str()).collect::<Vec<_>>() == ["$1", "∿1"]
    )));

    // The next snapshot from the server keeps the counts the client read.
    state.set_endpoint_snapshot(
        &endpoint_id,
        remote_snapshot(2, vec![claude("pane_1", 1, "✳ x")]),
    );
    assert_eq!(
        endpoint_agent_tokens(&state, &endpoint_id, "pane_1").len(),
        2
    );
}

#[test]
fn an_idle_herdr_pane_is_not_read_again() {
    let (mut state, endpoint_id) = herdr_machine();
    state.set_endpoint_snapshot(
        &endpoint_id,
        remote_snapshot(1, vec![claude("pane_1", 1, "✳ x")]),
    );
    let now = std::time::Instant::now();
    assert_eq!(state.take_remote_activity_reads(now).len(), 1);
    state.receive_remote_agent_screen(&endpoint_id, "pane_1", Some(FOOTER));
    // Same agent data again: nothing changed, so nothing is due.
    state.set_endpoint_snapshot(
        &endpoint_id,
        remote_snapshot(2, vec![claude("pane_1", 1, "✳ x")]),
    );
    let later = now + REMOTE_ACTIVITY_MIN_INTERVAL * 10;
    assert_eq!(state.remote_activity_due(later), None);
    assert!(state.take_remote_activity_reads(later).is_empty());
    assert_eq!(
        state.timer_delay(later),
        crate::client::shell::CLIENT_IDLE_TICK
    );
}

#[test]
fn changes_inside_the_interval_are_read_once_when_it_passes() {
    let (mut state, endpoint_id) = herdr_machine();
    state.set_endpoint_snapshot(
        &endpoint_id,
        remote_snapshot(1, vec![claude("pane_1", 1, "✳ a")]),
    );
    let start = std::time::Instant::now();
    assert_eq!(state.take_remote_activity_reads(start).len(), 1);
    state.receive_remote_agent_screen(&endpoint_id, "pane_1", Some(FOOTER));

    for (revision, title) in [(2, "◐ b"), (3, "◑ c"), (4, "◒ d")] {
        state.set_endpoint_snapshot(
            &endpoint_id,
            remote_snapshot(revision, vec![claude("pane_1", 1, title)]),
        );
    }
    let soon = start + std::time::Duration::from_millis(500);
    assert!(
        state.take_remote_activity_reads(soon).is_empty(),
        "throttled"
    );
    let trailing = start + REMOTE_ACTIVITY_MIN_INTERVAL;
    assert_eq!(state.remote_activity_due(soon), Some(trailing));
    assert_eq!(
        state.timer_delay(soon),
        trailing
            .saturating_duration_since(soon)
            .min(crate::client::shell::CLIENT_IDLE_TICK)
    );
    let almost = trailing - std::time::Duration::from_millis(200);
    assert_eq!(
        state.timer_delay(almost),
        std::time::Duration::from_millis(200)
    );
    assert_eq!(state.take_remote_activity_reads(trailing).len(), 1);
    state.receive_remote_agent_screen(&endpoint_id, "pane_1", Some(FOOTER));
    assert_eq!(state.remote_activity_due(trailing), None, "read once");
}

#[test]
fn servers_that_send_activity_are_never_read() {
    let (mut state, endpoint_id) = herdr_machine();
    state.set_endpoint_background_activity_supported(&endpoint_id, true);
    state.set_endpoint_snapshot(
        &endpoint_id,
        remote_snapshot(1, vec![claude("pane_1", 1, "✳ x")]),
    );
    let now = std::time::Instant::now();
    assert_eq!(state.remote_activity_due(now), None);
    assert!(state.take_remote_activity_reads(now).is_empty());

    // Local is always an oasis server.
    state.set_endpoint_background_activity_supported(&ClientEndpointId::Local, false);
    state.set_snapshot(remote_snapshot(1, vec![claude("pane_1", 1, "✳ x")]));
    assert!(state.take_remote_activity_reads(now).is_empty());
}

#[test]
fn only_claude_panes_are_read_and_departed_panes_are_dropped() {
    let (mut state, endpoint_id) = herdr_machine();
    let mut other = claude("pane_2", 1, "pi");
    other.agent = Some("pi".into());
    state.set_endpoint_snapshot(
        &endpoint_id,
        remote_snapshot(1, vec![claude("pane_1", 1, "✳ x"), other]),
    );
    let now = std::time::Instant::now();
    assert_eq!(
        state.take_remote_activity_reads(now),
        vec![(endpoint_id.clone(), "pane_1".to_owned())]
    );
    state.set_endpoint_snapshot(&endpoint_id, remote_snapshot(2, Vec::new()));
    // The in-flight pane left; its late answer changes nothing.
    assert!(!state.receive_remote_agent_screen(&endpoint_id, "pane_1", Some(FOOTER)));
    assert_eq!(state.remote_activity_due(now), None);
}

#[test]
fn an_unavailable_read_is_silent_and_stops_further_reads() {
    let (mut state, endpoint_id) = herdr_machine();
    state.set_endpoint_snapshot(
        &endpoint_id,
        remote_snapshot(1, vec![claude("pane_1", 1, "✳ x")]),
    );
    let now = std::time::Instant::now();
    assert_eq!(state.take_remote_activity_reads(now).len(), 1);
    assert!(!state.receive_remote_agent_screen(&endpoint_id, "pane_1", None));
    assert!(state.visible_endpoint_notice.is_none());
    assert!(state.endpoint_error.is_none());
    state.set_endpoint_snapshot(
        &endpoint_id,
        remote_snapshot(2, vec![claude("pane_1", 2, "◐ y")]),
    );
    let later = now + REMOTE_ACTIVITY_MIN_INTERVAL * 2;
    assert_eq!(state.remote_activity_due(later), None);
    assert!(endpoint_agent_tokens(&state, &endpoint_id, "pane_1").is_empty());
}

/// A goal or loop pane always shows the mode's glyph; working is yellow,
/// needing you the plain red mark, otherwise it takes the mode's color. A
/// working goal is a filled pink glyph, and done shows as idle. A goal just
/// met is a bold pink check until the next prompt. Between turns while
/// subagents run, the chat itself is idle: a grey robot; while it works, a
/// filled yellow one.
#[test]
fn goal_and_loop_panes_always_show_their_glyph() {
    use crate::config::StatusIndicatorStyle::Symbols;
    use crate::ui::AgentMode::{Goal, GoalAchieved, GoalFailed, Loop, Subagents};
    use ratatui::style::{Color, Style};
    let palette = Palette::catppuccin();
    let icon = |status, mode| agent_status_icon(status, mode, Symbols, &palette);
    let fg = |color: Color| Style::default().fg(color);
    let (goal, wakeup) = (Goal.color().ratatui(), Loop.color().ratatui());
    assert_eq!(
        icon(AgentStatus::Working, Some(Loop)),
        ("↻", fg(palette.yellow))
    );
    assert_eq!(icon(AgentStatus::Idle, Some(Loop)), ("↻", fg(wakeup)));
    assert_eq!(icon(AgentStatus::Done, Some(Loop)), ("↻", fg(wakeup)));
    assert_eq!(
        icon(AgentStatus::Blocked, Some(Loop)),
        ("×", fg(palette.red))
    );
    assert_eq!(icon(AgentStatus::Working, Some(Goal)), ("◉", fg(goal)));
    assert_eq!(icon(AgentStatus::Idle, Some(Goal)), ("◎", fg(goal)));
    assert_eq!(icon(AgentStatus::Done, Some(Goal)), ("◎", fg(goal)));
    let achieved = ("✓", fg(goal).add_modifier(ratatui::style::Modifier::BOLD));
    assert_eq!(icon(AgentStatus::Done, Some(GoalAchieved)), achieved);
    assert_eq!(icon(AgentStatus::Idle, Some(GoalAchieved)), achieved);
    let failed = (
        "✘",
        fg(GoalFailed.color().ratatui()).add_modifier(ratatui::style::Modifier::BOLD),
    );
    assert_eq!(icon(AgentStatus::Done, Some(GoalFailed)), failed);
    assert_eq!(icon(AgentStatus::Idle, Some(GoalFailed)), failed);
    assert_ne!(GoalFailed.color(), GoalAchieved.color());
    assert_eq!(
        icon(AgentStatus::Working, Some(GoalAchieved)),
        ("◐", fg(palette.yellow))
    );
    assert_eq!(
        icon(AgentStatus::Blocked, Some(Goal)),
        ("×", fg(palette.red))
    );
    // The outlined robot (nf-md-robot_outline, U+F167A), grey like idle.
    let robot = ("\u{f167a}", fg(palette.overlay0));
    assert_eq!(Subagents.glyph(), robot.0);
    assert_eq!(icon(AgentStatus::Idle, Some(Subagents)), robot);
    assert_eq!(icon(AgentStatus::Done, Some(Subagents)), robot);
    assert_eq!(
        icon(AgentStatus::Working, Some(Subagents)),
        ("\u{f06a9}", fg(palette.yellow))
    );
    assert_eq!(
        icon(AgentStatus::Blocked, Some(Subagents)),
        ("×", fg(palette.red))
    );
    assert_eq!(icon(AgentStatus::Working, None), ("◐", fg(palette.yellow)));
    assert_eq!(icon(AgentStatus::Idle, None), ("○", fg(palette.overlay0)));
    assert_eq!(icon(AgentStatus::Done, None), ("✓", fg(palette.teal)));
}

/// A space name that is no goal or loop draws bold in the theme's brightest
/// text, which on the terminal theme (text `Reset`) is bright white; a goal's
/// takes the goal's color, and a robot pane's stays bright.
#[test]
fn plain_space_names_draw_bold_bright_white_on_the_terminal_theme() {
    use crate::ui::{AgentMode, ResolvedToken, ResolvedTokenKind};
    use ratatui::{buffer::Buffer, layout::Rect, style::Color, style::Modifier};
    let mut config = ClientShellConfig::from_config(&Config::default());
    config.palette = Palette::terminal();
    assert_eq!(config.palette.text, Color::Reset);
    let name_cell = |mode: Option<AgentMode>| {
        let row = crate::client::model::AgentRow {
            pane_id: "pane_1".into(),
            status: AgentStatus::Idle,
            mode,
            focused: false,
            rows: vec![vec![
                ResolvedToken {
                    kind: ResolvedTokenKind::StateIcon,
                    style: Default::default(),
                },
                ResolvedToken {
                    kind: ResolvedTokenKind::Workspace("alexapp".into()),
                    style: Default::default(),
                },
            ]],
        };
        let rect = Rect::new(0, 0, 30, 1);
        let mut buffer = Buffer::empty(rect);
        super::agent_sidebar::render_agent_row(&mut buffer, rect, &row, &config);
        let x = (0..rect.width)
            .find(|x| buffer[(*x, 0)].symbol() == "a")
            .expect("the space name is drawn");
        let cell = &buffer[(x, 0)];
        (cell.fg, cell.modifier.contains(Modifier::BOLD))
    };
    assert_eq!(name_cell(None), (Color::White, true));
    assert_eq!(name_cell(Some(AgentMode::Subagents)), (Color::White, true));
    assert_eq!(
        name_cell(Some(AgentMode::Goal)),
        (AgentMode::Goal.color().ratatui(), true)
    );
}

/// A reconnect learns what the server supports after the client already holds
/// its snapshot; idle panes send nothing new, so they must be read anyway.
#[test]
fn panes_known_before_the_capability_is_learned_are_still_read() {
    let (mut state, endpoint_id) = herdr_machine();
    state.set_endpoint_snapshot(
        &endpoint_id,
        remote_snapshot(1, vec![claude("pane_1", 1, "✳ x")]),
    );
    let now = std::time::Instant::now();
    assert_eq!(state.take_remote_activity_reads(now).len(), 1);
    assert!(state.receive_remote_agent_screen(&endpoint_id, "pane_1", Some(FOOTER)));

    // The window reconnects: the server says again it lacks the capability.
    state.set_endpoint_background_activity_supported(&endpoint_id, false);
    assert!(
        !endpoint_agent_tokens(&state, &endpoint_id, "pane_1").is_empty(),
        "what was read stands until it is read again"
    );
    assert_eq!(
        state.take_remote_activity_reads(now + REMOTE_ACTIVITY_MIN_INTERVAL),
        vec![(endpoint_id.clone(), "pane_1".to_owned())]
    );
}

/// Every agent with a screen reader is read, each with its own reader.
#[test]
fn herdr_codex_panes_are_read_with_the_codex_reader() {
    let (mut state, endpoint_id) = herdr_machine();
    let mut codex = claude("pane_1", 1, "work");
    codex.agent = Some("codex".into());
    let mut shell = claude("pane_2", 1, "zsh");
    shell.agent = None;
    state.set_endpoint_snapshot(&endpoint_id, remote_snapshot(1, vec![codex, shell]));
    let now = std::time::Instant::now();
    assert_eq!(
        state.take_remote_activity_reads(now),
        vec![(endpoint_id.clone(), "pane_1".to_owned())],
        "panes without a reader are never read"
    );
    let screen = "  Worked for 3s • 11:14 AM\n  2 background terminals running · /ps to view\n\n› Ask Codex to do anything\n  GPT-5.6-Luna low · /var/tmp/work";
    assert!(state.receive_remote_agent_screen(&endpoint_id, "pane_1", Some(screen)));
    assert_eq!(
        endpoint_agent_tokens(&state, &endpoint_id, "pane_1"),
        vec![("oasis:shells".to_owned(), "2".to_owned())]
    );
}
