//! The embeddable client core against a real `oasis server` in isolated dirs.

#![cfg(target_os = "linux")]

pub mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use oasis_core::embed::{
    ClientCore, CoreEvent, CoreOptions, EndpointId, EndpointStatus, MachineSource, SurfaceUpdate,
};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use support::{register_runtime_dir, register_spawned_herdr_pid, wait_for_socket};

struct Server {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    base: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        support::stop_spawned_herdr(&mut *self.child);
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "oasis-dev"
    } else {
        "oasis"
    }
}

/// Starts `oasis server` in fresh dirs and points this test process's
/// environment at it, so the core finds its client socket.
fn start_server() -> Server {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = PathBuf::from(format!(
        "/tmp/oasis-embed-core-{}-{nanos}",
        std::process::id()
    ));
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    fs::create_dir_all(config_home.join(app_dir_name())).unwrap();
    fs::create_dir_all(&runtime_dir).unwrap();
    register_runtime_dir(&runtime_dir);
    fs::write(
        config_home.join(app_dir_name()).join("config.toml"),
        "onboarding = false\n",
    )
    .unwrap();

    // nextest runs each test in its own process, so this environment is the test's own.
    std::env::set_var("XDG_CONFIG_HOME", &config_home);
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    std::env::set_var("XDG_STATE_HOME", base.join("state"));
    std::env::set_var("HERDR_SOCKET_PATH", &api_socket);
    std::env::remove_var("HERDR_CLIENT_SOCKET_PATH");
    std::env::remove_var("HERDR_ENV");

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
    cmd.env("XDG_CONFIG_HOME", &config_home);
    cmd.env("XDG_RUNTIME_DIR", &runtime_dir);
    cmd.env("XDG_STATE_HOME", base.join("state"));
    cmd.env("HERDR_SOCKET_PATH", &api_socket);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");
    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(
        &oasis_core::embed::wire::client_socket_path(),
        Duration::from_secs(10),
    );
    Server {
        _master: pair.master,
        child,
        base,
    }
}

fn start_core() -> ClientCore {
    ClientCore::start(CoreOptions {
        surface_size: (80, 22),
        machines: MachineSource::None,
        server_exe: Some(PathBuf::from(env!("CARGO_BIN_EXE_oasis"))),
        ..CoreOptions::default()
    })
    .unwrap()
}

fn wait_readable(fd: i32, timeout: Duration) -> bool {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
    unsafe { libc::poll(&mut descriptor, 1, millis) > 0 }
}

/// Drains whenever the wake fd fires until `done` sees what it waits for.
fn drain_until(
    core: &ClientCore,
    timeout: Duration,
    mut done: impl FnMut(&oasis_core::embed::Drained) -> bool,
) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !wait_readable(
            core.wake_fd(),
            deadline.saturating_duration_since(Instant::now()),
        ) {
            break;
        }
        if done(&core.drain()) {
            return;
        }
    }
    panic!("the core did not deliver the expected update in {timeout:?}");
}

fn core_thread_switches() -> Option<u64> {
    for task in fs::read_dir("/proc/self/task").ok()?.flatten() {
        let path = task.path();
        let name = fs::read_to_string(path.join("comm")).unwrap_or_default();
        if name.trim() != "oasis-core" {
            continue;
        }
        return Some(context_switches(&path.join("status")));
    }
    None
}

fn context_switches(status: &Path) -> u64 {
    fs::read_to_string(status)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("ctxt_switches"))
        .filter_map(|line| line.split_whitespace().last()?.parse::<u64>().ok())
        .sum()
}

#[test]
fn core_streams_snapshot_and_surface_answers_requests_and_idles() {
    let _server = start_server();
    let core = start_core();
    let local = EndpointId::local();

    let mut online = false;
    let mut snapshot = false;
    let mut surface = false;
    drain_until(&core, Duration::from_secs(20), |drained| {
        for update in &drained.endpoints {
            assert_eq!(update.id, local);
            online |= update.status == Some(EndpointStatus::Online);
            snapshot |= update.snapshot.is_some();
            surface |= matches!(update.surface, Some(SurfaceUpdate::Full(_)));
        }
        online && snapshot && surface
    });

    let request: oasis_core::embed::wire::Request =
        serde_json::from_str(r#"{"id":"embed","method":"integration.list","params":{}}"#).unwrap();
    let id = core.request(&local, request.method);
    let mut answered = false;
    drain_until(&core, Duration::from_secs(10), |drained| {
        for event in &drained.events {
            if let CoreEvent::Response {
                id: response_id,
                result,
                ..
            } = event
            {
                assert_eq!(*response_id, id);
                assert!(result.is_ok(), "{result:?}");
                answered = true;
            }
        }
        answered
    });

    // Let startup output settle, then the core must stay quiet.
    std::thread::sleep(Duration::from_secs(2));
    let _ = core.drain();
    let before = core_thread_switches().expect("the core thread is running");
    let woke = wait_readable(core.wake_fd(), Duration::from_secs(4));
    let after = core_thread_switches().expect("the core thread is running");
    if woke {
        let drained = core.drain();
        panic!("an idle core published {drained:?}");
    }
    assert!(core.drain().is_empty(), "drain is empty while idle");
    assert!(
        after - before <= 2,
        "an idle core thread woke {} times in 4 s",
        after - before
    );
}

#[test]
fn a_core_started_without_a_server_reports_it() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let base = PathBuf::from(format!(
        "/tmp/oasis-embed-none-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(base.join("runtime")).unwrap();
    std::env::set_var("XDG_CONFIG_HOME", base.join("config"));
    std::env::set_var("XDG_RUNTIME_DIR", base.join("runtime"));
    std::env::set_var("HERDR_SOCKET_PATH", base.join("runtime").join("herdr.sock"));
    std::env::remove_var("HERDR_CLIENT_SOCKET_PATH");
    let core = ClientCore::start(CoreOptions {
        machines: MachineSource::None,
        server_exe: Some(base.join("no-such-oasis")),
        ..CoreOptions::default()
    })
    .unwrap();
    let mut attention = false;
    drain_until(&core, Duration::from_secs(15), |drained| {
        attention |= drained.endpoints.iter().any(|update| {
            matches!(&update.status, Some(EndpointStatus::Attention(message))
                if message.contains("did not start"))
        });
        attention
    });
    drop(core);
    let _ = fs::remove_dir_all(&base);
}
