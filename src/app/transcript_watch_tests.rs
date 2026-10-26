//! Transcript watches driven through the app the way a live pane drives them:
//! the screen goes working or idle, the app waits [`SETTLE`] and reads the
//! transcript on another thread, the result comes back as an event, and the
//! screen's own reading is published beside it.
//!
//! The Claude corpus (`tests/fixtures/claude-corpus/`) supplies the sessions:
//! a case's transcript is written out on its recorded timing, its screen is
//! shown when the turn's last message lands, and the pane must end showing
//! what the case's `[shown]` says.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::api::schema::{AgentStatus, BackgroundActivityInfo};
use crate::app::App;
use crate::detect::activity::BackgroundActivity;
use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::PaneId;

/// Mirrors the wait in `claude_transcript`: the read that follows a status
/// change lands this long after it.
const SETTLE: Duration = Duration::from_millis(400);

fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-corpus")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oasis-transcript-watch-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn test_app() -> (App, PaneId) {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &crate::config::Config::default(),
        crate::app::AppPolicy::TEST,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    let workspace = crate::workspace::Workspace::test_new("alex");
    let pane_id = workspace.tabs[0].root_pane;
    app.state.workspaces = vec![workspace];
    app.state.ensure_test_terminals();
    app.state.active = Some(0);
    (app, pane_id)
}

fn public_pane_id(app: &App, pane_id: PaneId) -> String {
    app.pane_info(0, pane_id).expect("pane").pane_id
}

fn api(app: &mut App, method: &str, params: serde_json::Value) {
    let request = serde_json::from_value(serde_json::json!({
        "id": "test",
        "method": method,
        "params": params,
    }))
    .expect("request");
    let response = app.handle_api_request(request);
    assert!(!response.contains("\"error\""), "{method}: {response}");
}

/// Claude's session hook reporting the transcript, as `herdr-agent-state.sh`
/// does.
fn report_claude_session(app: &mut App, pane_id: PaneId, transcript: &Path) {
    let pane = public_pane_id(app, pane_id);
    api(
        app,
        "pane.report_agent_session",
        serde_json::json!({
            "pane_id": pane,
            "source": "herdr:claude",
            "agent": "claude",
            "agent_session_path": transcript,
            "session_start_source": "startup",
        }),
    );
}

/// The screen detector's verdict, as the pane's detection loop sends it.
fn screen_status(app: &mut App, pane_id: PaneId, agent: Agent, state: AgentState) {
    app.handle_internal_event(AppEvent::StateChanged {
        pane_id,
        agent: Some(agent),
        state,
        visible_blocker: false,
        visible_working: state == AgentState::Working,
        process_exited: false,
        observed_at: Instant::now(),
    });
}

/// A screen as the pane's detection pass publishes it.
async fn show_screen(
    app: &mut App,
    pane_id: PaneId,
    agent: Agent,
    screen: &str,
    last: &mut BackgroundActivity,
) {
    let events = app.event_tx.clone();
    crate::pane::publish_background_activity(&events, pane_id, Some(agent), screen, false, last)
        .await;
    pump(app);
}

/// Hands the app every event that has arrived, as its loop does.
fn pump(app: &mut App) {
    while let Ok(event) = app.event_rx.try_recv() {
        app.handle_internal_event(event);
    }
}

/// Lets reads in flight finish: pumps for `span`.
async fn run_for(app: &mut App, span: Duration) {
    let until = Instant::now() + span;
    loop {
        pump(app);
        if Instant::now() >= until {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

fn shown(app: &App, pane_id: PaneId) -> BackgroundActivityInfo {
    app.pane_info(0, pane_id)
        .expect("pane")
        .background_activity
        .unwrap_or_default()
}

fn append(path: &Path, lines: &[&str]) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("transcript");
    for line in lines {
        writeln!(file, "{line}").expect("write transcript");
    }
}

fn case_lines(case: &str) -> Vec<String> {
    std::fs::read_to_string(corpus().join(case).join("transcript.jsonl"))
        .expect("corpus transcript")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn unix_seconds(line: &str) -> Option<f64> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    // `2026-10-26T13:58:02.876Z`
    let stamp = value.get("timestamp")?.as_str()?;
    let number = |range: std::ops::Range<usize>| stamp.get(range)?.parse::<u32>().ok();
    let month = time::Month::try_from(u8::try_from(number(5..7)?).ok()?).ok()?;
    let date = time::Date::from_calendar_date(
        i32::try_from(number(0..4)?).ok()?,
        month,
        u8::try_from(number(8..10)?).ok()?,
    )
    .ok()?;
    let clock = time::Time::from_hms(
        u8::try_from(number(11..13)?).ok()?,
        u8::try_from(number(14..16)?).ok()?,
        u8::try_from(number(17..19)?).ok()?,
    )
    .ok()?;
    let millis = number(20..23).unwrap_or(0);
    let seconds = time::PrimitiveDateTime::new(date, clock)
        .assume_utc()
        .unix_timestamp();
    Some(seconds as f64 + f64::from(millis) / 1000.0)
}

fn record_type(line: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .get("type")?
        .as_str()
        .map(str::to_owned)
}

/// Compressed replay time for `elapsed` seconds of recorded time since the
/// last status change. The read that change starts lands `SETTLE` later, so
/// that stretch runs in real time and every record lands on the same side
/// of the read as it did live; past it, time runs at half speed up to a cap.
fn replay_offset(elapsed: f64) -> Duration {
    let settle = SETTLE.as_secs_f64();
    let compressed = if elapsed <= settle {
        elapsed.max(0.0)
    } else {
        settle + ((elapsed - settle) / 2.0).min(2.5)
    };
    Duration::from_secs_f64(compressed)
}

/// The screen while the turn still works: the idle screen's prompt box and
/// footer under a spinner, with stock Claude's goal notice when a goal runs.
fn working_screen(idle: &str, goal: bool) -> String {
    let lines = idle.lines().collect::<Vec<_>>();
    let prompt = lines
        .iter()
        .rposition(|line| line.trim_start().starts_with('❯'))
        .expect("prompt");
    let top = lines[..prompt]
        .iter()
        .rposition(|line| line.trim_start().starts_with('─'))
        .expect("prompt box");
    let mut screen = vec!["✻ Crunching… (1m 8s · ↓ 2.1k tokens)"];
    if goal {
        screen.push("  ◎ /goal active (14h)");
    }
    screen.extend_from_slice(&lines[top..]);
    screen.join("\n")
}

struct ReplayCase {
    name: String,
    screen: String,
    expected: (bool, bool, u16, u16, u16),
}

/// Corpus cases that say what the pane shows and have a transcript to time.
fn replay_cases() -> Vec<ReplayCase> {
    let mut cases = std::fs::read_dir(corpus())
        .expect("corpus")
        .flatten()
        .filter(|entry| entry.path().join("transcript.jsonl").is_file())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let case: toml::Value =
                toml::from_str(&std::fs::read_to_string(entry.path().join("case.toml")).ok()?)
                    .ok()?;
            let shown = case.get("shown")?;
            let flag = |key: &str| shown.get(key).and_then(toml::Value::as_bool) == Some(true);
            let count = |key: &str| {
                shown
                    .get(key)
                    .and_then(toml::Value::as_integer)
                    .unwrap_or(0) as u16
            };
            let screen_case = shown
                .get("screen_from")
                .and_then(toml::Value::as_str)
                .unwrap_or(&name)
                .to_owned();
            let screen =
                std::fs::read_to_string(corpus().join(screen_case).join("screen.txt")).ok()?;
            Some(ReplayCase {
                expected: (
                    flag("goal"),
                    flag("goal_achieved"),
                    count("agents"),
                    count("shells"),
                    count("monitors"),
                ),
                name,
                screen,
            })
        })
        .collect::<Vec<_>>();
    cases.sort_by(|a, b| a.name.cmp(&b.name));
    cases
}

/// Replays one case; returns what went wrong.
async fn replay(case: &ReplayCase) -> Vec<String> {
    let mut failures = Vec::new();
    let lines = case_lines(&case.name);
    // The turn's last message is when its screen goes idle.
    let Some(idle_at) = lines
        .iter()
        .rposition(|line| record_type(line).as_deref() == Some("assistant"))
    else {
        return vec![format!("{}: no assistant message", case.name)];
    };
    // Records without a time land with the one before.
    let mut stamps = Vec::with_capacity(lines.len());
    let mut last = lines
        .iter()
        .find_map(|line| unix_seconds(line))
        .unwrap_or(0.0);
    for line in &lines {
        last = unix_seconds(line).unwrap_or(last);
        stamps.push(last);
    }

    let dir = scratch(&format!("replay-{}", case.name));
    let transcript = dir.join("session.jsonl");
    let (mut app, pane_id) = test_app();
    let mut screen = BackgroundActivity::default();
    // A new session: the hook reports the transcript before it exists, and
    // the first prompt comes later.
    report_claude_session(&mut app, pane_id, &transcript);
    run_for(&mut app, SETTLE * 2).await;
    let goal = case.expected.0 || case.expected.1;
    let working = working_screen(&case.screen, goal);
    show_screen(&mut app, pane_id, Agent::Claude, &working, &mut screen).await;
    screen_status(&mut app, pane_id, Agent::Claude, AgentState::Working);

    let mut changed_at = (Instant::now(), stamps[0]);
    for (index, line) in lines.iter().enumerate() {
        let due = changed_at.0 + replay_offset(stamps[index] - changed_at.1);
        while Instant::now() < due {
            pump(&mut app);
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        append(&transcript, &[line]);
        if index == idle_at {
            let before = shown(&app, pane_id);
            if goal && !before.goal {
                failures.push(format!(
                    "{}: no active goal shown while the turn worked: {before:?}",
                    case.name
                ));
            }
            show_screen(&mut app, pane_id, Agent::Claude, &case.screen, &mut screen).await;
            screen_status(&mut app, pane_id, Agent::Claude, AgentState::Idle);
            changed_at = (Instant::now(), stamps[index]);
        }
    }
    run_for(&mut app, SETTLE * 3).await;

    let info = shown(&app, pane_id);
    let found = (
        info.goal,
        info.goal_achieved,
        info.agents,
        info.shells,
        info.monitors,
    );
    if found != case.expected {
        failures.push(format!(
            "{}: shows (goal, achieved, agents, shells, monitors) {found:?}, expected {:?}",
            case.name, case.expected
        ));
    }
    let status = app.pane_info(0, pane_id).expect("pane").agent_status;
    if !matches!(status, AgentStatus::Idle | AgentStatus::Done) {
        failures.push(format!("{}: the chat is {status:?} once idle", case.name));
    }
    let _ = std::fs::remove_dir_all(dir);
    failures
}

/// The corpus on its recorded timing. A goal Claude records a few seconds
/// after the turn's last message lands after the read that follows the
/// screen going idle, so only the screen's `✔ Goal achieved` can show it.
#[tokio::test]
async fn corpus_sessions_replayed_in_time_show_what_their_screens_did() {
    let cases = replay_cases();
    assert!(
        cases.iter().any(|case| case.expected.1),
        "the corpus has no goal met after the idle read to replay"
    );
    let mut failures = Vec::new();
    for case in &cases {
        failures.extend(replay(case).await);
    }
    assert!(
        failures.is_empty(),
        "{} of {} timed replays disagree:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

/// A resumed session's earlier wakeups and jobs died with the earlier
/// process, so its watch starts at the end of the transcript; what is written
/// after counts.
#[tokio::test]
async fn a_resumed_session_is_watched_from_the_end_of_its_transcript() {
    let lines = case_lines("loop-hourly-job");
    let line = |number: usize| lines[number - 1].as_str();
    let dir = scratch("resumed");
    let transcript = dir.join("session.jsonl");
    // Up to the hourly job being created: a live loop, had it been read.
    append(&transcript, &(1..=30).map(line).collect::<Vec<_>>());
    let (mut app, pane_id) = test_app();
    report_claude_session(&mut app, pane_id, &transcript);
    screen_status(&mut app, pane_id, Agent::Claude, AgentState::Working);
    run_for(&mut app, SETTLE * 3).await;
    assert!(
        !shown(&app, pane_id).looping,
        "a loop from before the resume shows"
    );

    // After the resume, the /loop turn ends: its job died with the old
    // process, so nothing runs.
    append(&transcript, &(31..=137).map(line).collect::<Vec<_>>());
    screen_status(&mut app, pane_id, Agent::Claude, AgentState::Idle);
    run_for(&mut app, SETTLE * 3).await;
    assert_eq!(
        shown(&app, pane_id),
        BackgroundActivityInfo::default(),
        "the job from before the resume shows"
    );

    // Then a new /loop starts.
    append(&transcript, &(138..=142).map(line).collect::<Vec<_>>());
    screen_status(&mut app, pane_id, Agent::Claude, AgentState::Working);
    run_for(&mut app, SETTLE * 3).await;
    let info = shown(&app, pane_id);
    assert!(info.looping, "the loop started after the resume is missed");
    assert_eq!(info.loop_next, None, "its turn is running");
    let _ = std::fs::remove_dir_all(dir);
}

/// A Claude pane known only by its session id (handed over from a Herdr
/// server, or restored) finds `<claude config>/projects/<project>/<id>.jsonl`
/// when its status next changes.
#[tokio::test]
async fn a_pane_known_by_its_session_id_finds_its_transcript() {
    let _env = crate::integration::integration_env_lock();
    let dir = scratch("session-id");
    let session = "6f1d2c3b-0a9e-4f8d-b7c6-a5e4d3c2b1a0";
    let transcript = dir
        .join("claude/projects/-work-alexapp")
        .join(format!("{session}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::create_dir_all(dir.join("claude/projects/-work-other")).unwrap();
    let lines = case_lines("goal-met-then-typed-prompt");
    append(&transcript, &[lines[0].as_str()]);
    let previous = std::env::var_os("CLAUDE_CONFIG_DIR");
    std::env::set_var("CLAUDE_CONFIG_DIR", dir.join("claude"));

    let (mut app, pane_id) = test_app();
    screen_status(&mut app, pane_id, Agent::Claude, AgentState::Working);
    let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
        .attached_terminal_id
        .clone();
    let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
    assert!(terminal.claude_transcript.is_none());
    terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
        source: "herdr:claude".into(),
        agent: "claude".into(),
        session_ref: crate::agent_resume::AgentSessionRef::id(session).unwrap(),
    });
    screen_status(&mut app, pane_id, Agent::Claude, AgentState::Idle);
    let watched = app.state.terminals[&terminal_id]
        .claude_transcript
        .as_ref()
        .map(|watch| watch.path.clone());
    match previous {
        Some(value) => std::env::set_var("CLAUDE_CONFIG_DIR", value),
        None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
    }
    assert_eq!(watched, Some(transcript.clone()));
    run_for(&mut app, SETTLE * 2).await;

    // Read from its end on: the goal met afterwards shows.
    append(
        &transcript,
        &lines[1..7].iter().map(String::as_str).collect::<Vec<_>>(),
    );
    screen_status(&mut app, pane_id, Agent::Claude, AgentState::Working);
    run_for(&mut app, SETTLE * 2).await;
    assert!(shown(&app, pane_id).goal_achieved);
    let _ = std::fs::remove_dir_all(dir);
}

/// "Working" is the chat's own turn. Shells, monitors and subagents left
/// running after it show as background work; the chat stays idle.
#[tokio::test]
async fn background_work_never_makes_the_chat_working() {
    let screen = std::fs::read_to_string(corpus().join("screen-goal-shells-subagent/screen.txt"))
        .expect("corpus screen");
    let (mut app, pane_id) = test_app();
    screen_status(&mut app, pane_id, Agent::Claude, AgentState::Idle);
    let mut last = BackgroundActivity::default();
    show_screen(&mut app, pane_id, Agent::Claude, &screen, &mut last).await;
    let info = app.pane_info(0, pane_id).expect("pane");
    let background = info.background_activity.unwrap_or_default();
    assert_eq!((background.shells, background.agents), (2, 1));
    assert!(matches!(
        info.agent_status,
        AgentStatus::Idle | AgentStatus::Done
    ));
    let workspace = app.workspace_info(0);
    assert!(
        !matches!(workspace.agent_status, AgentStatus::Working),
        "the space works: {:?}",
        workspace.agent_status
    );
}

mod codex {
    use super::*;

    const PARENT: &str = "01a1753b-cd5d-7900-9d3f-3251e0833832";
    const OTHER: &str = "01a1753b-d000-7900-9d3f-3251e0833833";
    const CHILD: &str = "01a1753c-06df-75f0-8e36-46dbcea2c2cf";
    const OTHER_CHILD: &str = "01a1753c-0700-75f0-8e36-46dbcea2c2d0";
    const STARTED: &str =
        r#"{"type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#;

    // Shape captured from a live Codex CLI 0.164.0 session.
    fn spawn(parent: &str, child: &str) -> String {
        format!(
            r#"{{"type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"CollabAgentToolCall","tool":"spawn_agent","status":"completed","sender_thread_id":"{parent}","receiver_thread_ids":["{child}"]}}}}}}"#
        )
    }

    fn rollout(sessions: &Path, id: &str, lines: &[&str]) -> PathBuf {
        let dir = sessions.join("2026/10/25");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-10-25T18-43-35-{id}.jsonl"));
        append(&path, lines);
        path
    }

    fn watched(app: &App, pane_id: PaneId) -> Option<PathBuf> {
        let terminal_id = &app.state.workspaces[0].tabs[0].panes[&pane_id].attached_terminal_id;
        app.state.terminals[terminal_id]
            .codex_subagents
            .as_ref()
            .map(|watch| watch.path.clone())
    }

    fn set_title(app: &mut App, pane_id: PaneId, title: &str) {
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        let _ = terminal.set_terminal_title(Some(title.to_owned()));
        app.codex_title_changed(pane_id);
    }

    /// The pane is bound to the thread its title names and counts that
    /// thread's subagents. Hook reports come from Codex's shared app-server
    /// with whichever pane started it, so a report naming another thread
    /// moves nothing.
    #[tokio::test]
    async fn a_codex_pane_follows_the_thread_its_title_names_not_hook_reports() {
        let _env = crate::integration::integration_env_lock();
        let dir = scratch("codex-title");
        let sessions = dir.join("codex/sessions");
        let parent = rollout(&sessions, PARENT, &[&spawn(PARENT, CHILD)]);
        rollout(&sessions, CHILD, &[STARTED]);
        let other = rollout(
            &sessions,
            OTHER,
            &[&spawn(OTHER, OTHER_CHILD), &spawn(OTHER, CHILD)],
        );
        rollout(&sessions, OTHER_CHILD, &[STARTED]);
        let previous = std::env::var_os("CODEX_HOME");
        std::env::set_var("CODEX_HOME", dir.join("codex"));

        let (mut app, pane_id) = test_app();
        screen_status(&mut app, pane_id, Agent::Codex, AgentState::Working);
        assert_eq!(watched(&app, pane_id), None, "no title names a thread yet");
        // The hook names the other pane's thread.
        let pane = public_pane_id(&app, pane_id);
        api(
            &mut app,
            "pane.report_agent_session",
            serde_json::json!({
                "pane_id": pane,
                "source": "herdr:codex",
                "agent": "codex",
                "agent_session_id": OTHER,
                "session_start_source": "startup",
            }),
        );
        screen_status(&mut app, pane_id, Agent::Codex, AgentState::Idle);
        let after_hook = watched(&app, pane_id);
        set_title(
            &mut app,
            pane_id,
            &format!("⠦ Write the release notes | {}...", &PARENT[..29]),
        );
        let after_title = watched(&app, pane_id);
        run_for(&mut app, SETTLE * 2).await;
        match previous {
            Some(value) => std::env::set_var("CODEX_HOME", value),
            None => std::env::remove_var("CODEX_HOME"),
        }
        assert_eq!(after_hook, None, "a hook report bound the pane");
        assert_eq!(after_title, Some(parent));
        assert_ne!(after_title, Some(other));
        assert_eq!(shown(&app, pane_id).agents, 1);
        let info = app.pane_info(0, pane_id).expect("pane");
        assert_eq!(
            info.terminal_title_stripped.as_deref(),
            Some("Write the release notes"),
            "the thread id shows in the session name"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
