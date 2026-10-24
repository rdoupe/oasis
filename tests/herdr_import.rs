#![cfg(unix)]
//! A first-launch takeover of a running Herdr server: its panes move to an
//! oasis server, and Herdr's socket paths lead there afterwards.

pub mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use support::{
    cleanup_test_base, register_runtime_dir, register_spawned_herdr_pid,
    unregister_spawned_herdr_pid, wait_for_file, wait_for_socket, wait_until,
};

struct SpawnedServer {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedServer {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        support::stop_spawned_herdr(&mut *self.child);
        unregister_spawned_herdr_pid(pid);
    }
}

fn unique_test_dir() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("/tmp/hhi-{}-{n}", std::process::id()))
}

fn dir_names() -> (&'static str, &'static str) {
    if cfg!(debug_assertions) {
        ("herdr-dev", "oasis-dev")
    } else {
        ("herdr", "oasis")
    }
}

/// A stand-in Herdr server: serves Herdr's socket paths, as a Herdr install
/// beside oasis does.
fn spawn_herdr_server(base: &Path, herdr_api: &Path) -> SpawnedServer {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_oasis"));
    support::isolate_herdr_test_process(&mut cmd);
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", base.join("config"));
    cmd.env("XDG_STATE_HOME", base.join("state"));
    cmd.env("XDG_RUNTIME_DIR", base.join("runtime"));
    cmd.env("HERDR_SOCKET_PATH", herdr_api);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env_remove("HERDR_CONFIG_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");
    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    SpawnedServer {
        _master: pair.master,
        child,
    }
}

fn request(socket: &Path, request: serde_json::Value) -> serde_json::Value {
    let mut stream = UnixStream::connect(socket).expect("connect to API socket");
    writeln!(stream, "{request}").unwrap();
    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response).unwrap();
    serde_json::from_str(&response).expect("JSON response")
}

fn run_in_pane(socket: &Path, pane_id: &str, command: &str) {
    let response = request(
        socket,
        serde_json::json!({
            "id": "run", "method": "pane.send_input",
            "params": {"pane_id": pane_id, "text": command, "keys": ["Enter"]},
        }),
    );
    assert_eq!(response["result"]["type"], "ok", "{response}");
}

fn oasis_command(base: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_oasis"));
    command
        .env("XDG_CONFIG_HOME", base.join("config"))
        .env("XDG_STATE_HOME", base.join("state"))
        .env("XDG_RUNTIME_DIR", base.join("runtime"))
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_CLIENT_SOCKET_PATH")
        .env_remove("HERDR_CONFIG_PATH")
        .env_remove("HERDR_SESSION")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_ENV");
    command
}

#[test]
fn import_takes_over_a_running_herdr_server_and_keeps_its_panes() {
    let base = unique_test_dir();
    let (herdr_dir, oasis_dir) = dir_names();
    let herdr_config = base.join("config").join(herdr_dir);
    let herdr_api = herdr_config.join("herdr.sock");
    let herdr_client = herdr_config.join("herdr-client.sock");
    let oasis_api = base.join("config").join(oasis_dir).join("herdr.sock");
    let oasis_client = base
        .join("config")
        .join(oasis_dir)
        .join("herdr-client.sock");
    fs::create_dir_all(&herdr_config).unwrap();
    fs::write(herdr_config.join("config.toml"), "onboarding = false\n").unwrap();
    register_runtime_dir(&base.join("runtime"));

    let herdr = spawn_herdr_server(&base, &herdr_api);
    wait_for_socket(&herdr_api, Duration::from_secs(10));
    let created = request(
        &herdr_api,
        serde_json::json!({
            "id": "create", "method": "workspace.create",
            "params": {"cwd": base, "focus": true, "label": "from-herdr"},
        }),
    );
    let pane_id = created["result"]["root_pane"]["pane_id"]
        .as_str()
        .expect("root pane")
        .to_owned();
    let before = base.join("pid-before");
    run_in_pane(
        &herdr_api,
        &pane_id,
        &format!("echo $$ > {}", before.display()),
    );
    wait_for_file(&before, Duration::from_secs(10));

    let output = oasis_command(&base)
        .args(["import-herdr", "--apply", "settings,server"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "import failed: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("Herdr's panes now run under oasis"),
        "{stdout}"
    );
    assert!(base
        .join("config")
        .join(oasis_dir)
        .join("config.toml")
        .is_file());

    // The oasis server serves the pane that ran under Herdr, with the same shell.
    wait_for_socket(&oasis_api, Duration::from_secs(10));
    let panes = request(
        &oasis_api,
        serde_json::json!({"id": "list", "method": "pane.list", "params": {}}),
    );
    let moved = panes["result"]["panes"]
        .as_array()
        .and_then(|panes| panes.first())
        .and_then(|pane| pane["pane_id"].as_str())
        .expect("a pane survived the takeover")
        .to_owned();
    let after = base.join("pid-after");
    run_in_pane(
        &oasis_api,
        &moved,
        &format!("echo $$ > {}", after.display()),
    );
    wait_for_file(&after, Duration::from_secs(10));
    assert!(wait_until(
        Duration::from_secs(5),
        Duration::from_millis(25),
        || fs::read_to_string(&after).is_ok_and(|pid| !pid.trim().is_empty())
    ));
    assert_eq!(
        fs::read_to_string(&before).unwrap().trim(),
        fs::read_to_string(&after).unwrap().trim(),
        "the same shell process kept running"
    );

    // Herdr's socket paths now reach the oasis server.
    assert_eq!(fs::read_link(&herdr_api).unwrap(), oasis_api);
    assert_eq!(fs::read_link(&herdr_client).unwrap(), oasis_client);
    let pong = request(
        &herdr_api,
        serde_json::json!({"id": "ping", "method": "ping", "params": {}}),
    );
    assert_eq!(pong["result"]["type"], "pong", "{pong}");

    // The decision is remembered, so the next launch does not offer it again.
    let marker = base.join("state").join(oasis_dir).join("herdr-import.json");
    assert!(fs::read_to_string(marker).unwrap().contains("imported"));

    let _ = request(
        &oasis_api,
        serde_json::json!({"id": "stop", "method": "server.stop", "params": {}}),
    );
    drop(herdr);
    cleanup_test_base(&base);
}

#[test]
fn import_without_a_running_herdr_server_refuses_the_takeover_but_copies_the_rest() {
    let base = unique_test_dir();
    let (herdr_dir, oasis_dir) = dir_names();
    let herdr_config = base.join("config").join(herdr_dir);
    fs::create_dir_all(&herdr_config).unwrap();
    fs::write(
        herdr_config.join("config.toml"),
        "[ui]\naccent = \"blue\"\n",
    )
    .unwrap();
    fs::write(herdr_config.join("session.json"), "{}").unwrap();

    let output = oasis_command(&base)
        .args(["import-herdr", "--apply", "settings,windows,server"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("Herdr's server isn't running"), "{stderr}");
    let oasis_config = base.join("config").join(oasis_dir);
    assert!(fs::read_to_string(oasis_config.join("config.toml"))
        .unwrap()
        .contains("accent"));
    assert!(oasis_config.join("session.json").is_file());
    assert!(!oasis_config.join("herdr.sock").exists());
    let _ = fs::remove_dir_all(base);
}

#[test]
fn cli_commands_never_import_or_prompt() {
    let base = unique_test_dir();
    let (herdr_dir, oasis_dir) = dir_names();
    let herdr_config = base.join("config").join(herdr_dir);
    fs::create_dir_all(&herdr_config).unwrap();
    fs::write(
        herdr_config.join("config.toml"),
        "[ui]\naccent = \"blue\"\n",
    )
    .unwrap();

    let output = oasis_command(&base).arg("--version").output().unwrap();
    let _ = output;
    assert!(
        !base
            .join("config")
            .join(oasis_dir)
            .join("config.toml")
            .exists(),
        "nothing is copied without the user choosing it"
    );
    let _ = fs::remove_dir_all(base);
}

#[test]
fn imported_windows_restore_in_the_oasis_server() {
    let base = unique_test_dir();
    let (herdr_dir, oasis_dir) = dir_names();
    // A server under another config home writes the session Herdr would.
    let elsewhere = base.join("elsewhere");
    let saved_api = elsewhere.join("herdr.sock");
    fs::create_dir_all(elsewhere.join(oasis_dir)).unwrap();
    fs::write(
        elsewhere.join(oasis_dir).join("config.toml"),
        "onboarding = false\n",
    )
    .unwrap();
    register_runtime_dir(&base.join("runtime"));
    let mut saver = {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_oasis"));
        support::isolate_herdr_test_process(&mut cmd);
        cmd.arg("server");
        cmd.env("XDG_CONFIG_HOME", &elsewhere);
        cmd.env("XDG_STATE_HOME", elsewhere.join("state"));
        cmd.env("XDG_RUNTIME_DIR", base.join("runtime"));
        cmd.env("HERDR_SOCKET_PATH", &saved_api);
        cmd.env("SHELL", "/bin/sh");
        let child = pair.slave.spawn_command(cmd).unwrap();
        register_spawned_herdr_pid(child.process_id());
        SpawnedServer {
            _master: pair.master,
            child,
        }
    };
    wait_for_socket(&saved_api, Duration::from_secs(10));
    let workspace_cwd = base.join("project");
    fs::create_dir_all(&workspace_cwd).unwrap();
    request(
        &saved_api,
        serde_json::json!({
            "id": "create", "method": "workspace.create",
            "params": {"cwd": workspace_cwd, "focus": true, "label": "herdr-space"},
        }),
    );
    request(
        &saved_api,
        serde_json::json!({"id": "stop", "method": "server.stop", "params": {}}),
    );
    let saved_session = elsewhere.join(oasis_dir).join("session.json");
    assert!(wait_until(
        Duration::from_secs(10),
        Duration::from_millis(25),
        || fs::read_to_string(&saved_session).is_ok_and(|text| text.contains("herdr-space"))
    ));
    support::stop_spawned_herdr(&mut *saver.child);
    let _ = saver.child.wait();
    let herdr_config = base.join("config").join(herdr_dir);
    fs::create_dir_all(&herdr_config).unwrap();
    fs::copy(&saved_session, herdr_config.join("session.json")).unwrap();

    let output = oasis_command(&base)
        .args(["import-herdr", "--apply", "windows"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let oasis_api = base.join("config").join(oasis_dir).join("herdr.sock");
    fs::write(
        base.join("config").join(oasis_dir).join("config.toml"),
        "onboarding = false\n",
    )
    .unwrap();
    let oasis = spawn_herdr_server(&base, &oasis_api);
    wait_for_socket(&oasis_api, Duration::from_secs(10));
    let workspaces = request(
        &oasis_api,
        serde_json::json!({"id": "list", "method": "workspace.list", "params": {}}),
    );
    let restored = workspaces["result"]["workspaces"]
        .as_array()
        .expect("workspaces")
        .iter()
        .any(|workspace| workspace["label"] == "herdr-space");
    assert!(restored, "Herdr's window restored: {workspaces}");
    drop(oasis);
    cleanup_test_base(&base);
}
