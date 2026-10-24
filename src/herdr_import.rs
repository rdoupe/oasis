//! First-launch import from a Herdr install.
//!
//! Oasis is a fork of Herdr with its own config and state directories. When a
//! Herdr install sits beside it, the first client launch offers to bring it
//! over: settings, saved machines, the open windows (the session layout) and,
//! optionally, the running Herdr server itself. Nothing is copied without the
//! user choosing it, and the decision is remembered so the offer shows once
//! (`oasis import-herdr` offers it again).
//!
//! Taking over the server uses Herdr's own live handoff: Herdr's server hands
//! its panes to an oasis server, which then serves Oasis's sockets, and Herdr's
//! socket paths become links to them. Panes keep running, agent hooks inside
//! them keep reaching their server, and machines that connect to this one
//! through Herdr's paths reach the oasis server instead.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// `oasis import-herdr` forgets the decision and offers the import again.
pub const REIMPORT_COMMAND: &str = "import-herdr";
/// Remembers the import decision in the oasis state directory.
pub const MARKER_FILE: &str = "herdr-import.json";
/// Settings kept under the config directory.
const SETTINGS_ENTRIES: &[&str] = &["config.toml", "agent-detection", "plugins", "plugins.json"];
/// Saved machines and client layout kept under the state directory.
const REMOTES_ENTRIES: &[&str] = &["client", "client-shell"];
/// The persisted session layout, kept under the config directory.
const WINDOWS_ENTRIES: &[&str] = &["session.json", "session-history.json"];
/// The wrapper Herdr's server runs as its handoff import executable.
const TAKEOVER_SHIM: &str = "herdr-takeover";
const SERVER_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(60);
const SOCKET_WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportItem {
    Settings,
    Remotes,
    Windows,
    Server,
}

pub const IMPORT_ITEMS: [ImportItem; 4] = [
    ImportItem::Settings,
    ImportItem::Remotes,
    ImportItem::Windows,
    ImportItem::Server,
];

impl ImportItem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Settings => "Settings",
            Self::Remotes => "Saved machines",
            Self::Windows => "Windows",
            Self::Server => "Take over Herdr's server",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::Settings => "config, keys, agent detection rules, plugins",
            Self::Remotes => "remote machines and sidebar layout",
            Self::Windows => "spaces, tabs and panes, reopened as new shells",
            Self::Server => "keep running panes; Herdr's sockets lead to oasis",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    Available,
    Unavailable(String),
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// The directories an import reads from and writes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportDirs {
    pub oasis_config: PathBuf,
    pub oasis_state: PathBuf,
    pub herdr_config: PathBuf,
    pub herdr_state: PathBuf,
}

impl ImportDirs {
    /// Herdr's directories sit beside Oasis's: `~/.config/herdr` next to
    /// `~/.config/oasis`, and the same under the state directory.
    pub fn current() -> Option<Self> {
        let oasis_config = crate::config::config_dir();
        let oasis_state = crate::config::state_dir();
        let legacy = legacy_app_dir_name();
        Some(Self {
            herdr_config: oasis_config.parent()?.join(legacy),
            herdr_state: oasis_state.parent()?.join(legacy),
            oasis_config,
            oasis_state,
        })
    }

    fn herdr_api_socket(&self) -> PathBuf {
        self.herdr_config.join("herdr.sock")
    }

    fn herdr_client_socket(&self) -> PathBuf {
        self.herdr_config.join("herdr-client.sock")
    }
}

pub(crate) fn legacy_app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

/// What a probe of Herdr's API socket found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerProbe {
    NotRunning,
    Running {
        version: String,
        protocol: u32,
        live_handoff: bool,
    },
}

/// A Herdr install that can be imported, with what each item can do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HerdrInstall {
    pub dirs: ImportDirs,
    pub settings: Availability,
    pub remotes: Availability,
    pub windows: Availability,
    pub server: Availability,
}

impl HerdrInstall {
    pub fn availability(&self, item: ImportItem) -> &Availability {
        match item {
            ImportItem::Settings => &self.settings,
            ImportItem::Remotes => &self.remotes,
            ImportItem::Windows => &self.windows,
            ImportItem::Server => &self.server,
        }
    }

    /// Everything available starts selected, except taking over the server.
    pub fn default_selected(&self, item: ImportItem) -> bool {
        item != ImportItem::Server && self.availability(item).is_available()
    }

    pub fn default_plan(&self) -> ImportPlan {
        let mut plan = ImportPlan::default();
        for item in IMPORT_ITEMS {
            plan.set(item, self.default_selected(item));
        }
        plan
    }
}

/// The items the user chose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImportPlan {
    pub settings: bool,
    pub remotes: bool,
    pub windows: bool,
    pub server: bool,
}

impl ImportPlan {
    pub fn get(&self, item: ImportItem) -> bool {
        match item {
            ImportItem::Settings => self.settings,
            ImportItem::Remotes => self.remotes,
            ImportItem::Windows => self.windows,
            ImportItem::Server => self.server,
        }
    }

    pub fn set(&mut self, item: ImportItem, value: bool) {
        match item {
            ImportItem::Settings => self.settings = value,
            ImportItem::Remotes => self.remotes = value,
            ImportItem::Windows => self.windows = value,
            ImportItem::Server => self.server = value,
        }
    }

    pub fn is_empty(&self) -> bool {
        !(self.settings || self.remotes || self.windows || self.server)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Imported,
    Skipped,
}

#[derive(Serialize, Deserialize)]
struct Marker {
    decision: Decision,
    version: String,
}

pub fn decision(state_dir: &Path) -> Option<Decision> {
    let text = std::fs::read_to_string(state_dir.join(MARKER_FILE)).ok()?;
    serde_json::from_str::<Marker>(&text)
        .ok()
        .map(|marker| marker.decision)
}

pub fn record_decision(state_dir: &Path, decision: Decision) -> io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let marker = Marker {
        decision,
        version: crate::build_info::version().to_owned(),
    };
    std::fs::write(
        state_dir.join(MARKER_FILE),
        serde_json::to_string_pretty(&marker).map_err(io::Error::other)?,
    )
}

/// Forgets the decision so the next client launch offers the import again.
pub fn forget_decision(state_dir: &Path) -> io::Result<()> {
    match std::fs::remove_file(state_dir.join(MARKER_FILE)) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Finds a Herdr install to offer, or `None` when the decision was already
/// made, no install exists, or it has nothing to import.
pub fn detect(dirs: &ImportDirs, probe: impl FnOnce(&Path) -> ServerProbe) -> Option<HerdrInstall> {
    if decision(&dirs.oasis_state).is_some() {
        return None;
    }
    if !dirs.herdr_config.is_dir() && !dirs.herdr_state.is_dir() {
        return None;
    }
    let found = |root: &Path, entries: &[&str], missing: &str| {
        if entries.iter().any(|entry| root.join(entry).exists()) {
            Availability::Available
        } else {
            Availability::Unavailable(missing.to_owned())
        }
    };
    let settings = found(
        &dirs.herdr_config,
        SETTINGS_ENTRIES,
        "no Herdr settings found",
    );
    let remotes = found(
        &dirs.herdr_state,
        &["client/endpoints.json"],
        "no saved machines found",
    );
    let windows = found(
        &dirs.herdr_config,
        &["session.json"],
        "no saved Herdr windows",
    );
    let server = server_availability(dirs, probe(&dirs.herdr_api_socket()));
    let install = HerdrInstall {
        dirs: dirs.clone(),
        settings,
        remotes,
        windows,
        server,
    };
    IMPORT_ITEMS
        .iter()
        .any(|item| install.availability(*item).is_available())
        .then_some(install)
}

/// Detects an install beside the current oasis directories, probing Herdr's
/// running server over its API socket.
pub fn detect_current() -> Option<HerdrInstall> {
    let dirs = ImportDirs::current()?;
    detect(&dirs, probe_server)
}

pub fn probe_server(socket: &Path) -> ServerProbe {
    if !socket.exists() {
        return ServerProbe::NotRunning;
    }
    let client = crate::api::client::ApiClient::for_target(
        crate::api::client::ConnectionTarget::SocketPath(socket.to_path_buf()),
    );
    match client.status_with_timeout(SERVER_PROBE_TIMEOUT) {
        Ok(status) => ServerProbe::Running {
            version: status.version.unwrap_or_default(),
            protocol: status.protocol.unwrap_or_default(),
            live_handoff: status
                .capabilities
                .is_some_and(|capabilities| capabilities.live_handoff),
        },
        Err(_) => ServerProbe::NotRunning,
    }
}

/// Whether Herdr's running server can hand its panes to oasis.
pub fn server_availability(dirs: &ImportDirs, probe: ServerProbe) -> Availability {
    if !cfg!(unix) {
        return Availability::Unavailable("needs Linux or macOS".into());
    }
    if crate::api::socket_path() == dirs.herdr_api_socket() {
        return Availability::Unavailable("oasis is already using Herdr's server".into());
    }
    if std::fs::read_link(dirs.herdr_api_socket()).is_ok() {
        return Availability::Unavailable("Herdr's sockets already lead to oasis".into());
    }
    match probe {
        ServerProbe::NotRunning => Availability::Unavailable("Herdr's server isn't running".into()),
        ServerProbe::Running {
            version,
            live_handoff: false,
            ..
        } => Availability::Unavailable(format!("Herdr {version} can't hand off its panes")),
        ServerProbe::Running {
            version, protocol, ..
        } if protocol != crate::protocol::PROTOCOL_VERSION => Availability::Unavailable(format!(
            "Herdr {version} speaks protocol {protocol}; oasis speaks {}",
            crate::protocol::PROTOCOL_VERSION
        )),
        ServerProbe::Running { .. } => Availability::Available,
    }
}

static PENDING: Mutex<Option<(HerdrInstall, ImportPlan)>> = Mutex::new(None);

/// Queues an import for after the client exits: the oasis server it runs
/// against has to stop first, so its first-launch state does not overwrite
/// the imported session.
pub fn set_pending(install: HerdrInstall, plan: ImportPlan) {
    if let Ok(mut pending) = PENDING.lock() {
        *pending = Some((install, plan));
    }
}

pub fn take_pending() -> Option<(HerdrInstall, ImportPlan)> {
    PENDING.lock().ok()?.take()
}

/// Copies one directory's entries, replacing what is already there. Runtime
/// files (sockets, logs, handoff state) are never listed, so never copied.
fn copy_entries(source: &Path, destination: &Path, entries: &[&str]) -> io::Result<Vec<String>> {
    let mut copied = Vec::new();
    for entry in entries {
        let from = source.join(entry);
        let to = destination.join(entry);
        if from.is_dir() {
            if to.is_dir() {
                std::fs::remove_dir_all(&to)?;
            }
            crate::plugin_paths::copy_dir_all(&from, &to)?;
        } else if from.is_file() {
            std::fs::create_dir_all(destination)?;
            if *entry == "config.toml" && to.is_file() {
                std::fs::copy(&to, destination.join("config.toml.before-herdr-import"))?;
            }
            std::fs::copy(&from, &to)?;
        } else {
            continue;
        }
        copied.push((*entry).to_owned());
    }
    Ok(copied)
}

/// Copies the chosen settings, saved machines and windows. Taking over the
/// server is separate: it brings the windows with their running panes.
pub fn import_files(dirs: &ImportDirs, plan: ImportPlan) -> io::Result<Vec<String>> {
    let mut copied = Vec::new();
    if plan.settings {
        copied.extend(copy_entries(
            &dirs.herdr_config,
            &dirs.oasis_config,
            SETTINGS_ENTRIES,
        )?);
    }
    if plan.remotes {
        copied.extend(copy_entries(
            &dirs.herdr_state,
            &dirs.oasis_state,
            REMOTES_ENTRIES,
        )?);
    }
    if plan.windows && !plan.server {
        copied.extend(import_windows(dirs)?);
    }
    Ok(copied)
}

fn import_windows(dirs: &ImportDirs) -> io::Result<Vec<String>> {
    copy_entries(&dirs.herdr_config, &dirs.oasis_config, WINDOWS_ENTRIES)
}

/// The oasis sockets an import stops and the takeover server binds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OasisSockets {
    pub api: PathBuf,
    pub client: PathBuf,
}

impl OasisSockets {
    pub fn current() -> Self {
        Self {
            api: crate::api::socket_path(),
            client: crate::server::socket_paths::client_socket_path(),
        }
    }
}

/// What an import did, for printing once the client has exited.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub lines: Vec<String>,
    /// Whether the server takeover succeeded, when one was chosen.
    pub server_taken_over: Option<bool>,
}

/// Runs a chosen import with the client exited, and records the decision.
/// The caller starts the client again afterwards.
pub fn run_import(
    install: &HerdrInstall,
    plan: ImportPlan,
    sockets: &OasisSockets,
) -> ImportReport {
    let dirs = &install.dirs;
    let mut report = ImportReport::default();
    stop_oasis_server(sockets);
    match import_files(dirs, plan) {
        Ok(copied) if !copied.is_empty() => {
            report.lines.push(format!("copied {}", copied.join(", ")));
        }
        Ok(_) => {}
        Err(err) => report
            .lines
            .push(format!("could not copy Herdr's files: {err}")),
    }
    if plan.server {
        match take_over_server(dirs, sockets) {
            Ok(()) => {
                report.server_taken_over = Some(true);
                report
                    .lines
                    .push("Herdr's panes now run under oasis".into());
            }
            Err(err) => {
                report.server_taken_over = Some(false);
                report
                    .lines
                    .push(format!("Herdr's server was left running: {err}"));
                if plan.windows {
                    match import_windows(dirs) {
                        Ok(copied) if !copied.is_empty() => {
                            report.lines.push("copied Herdr's windows instead".into());
                        }
                        Ok(_) => {}
                        Err(err) => report
                            .lines
                            .push(format!("could not copy Herdr's windows: {err}")),
                    }
                }
            }
        }
    }
    if let Err(err) = record_decision(&dirs.oasis_state, Decision::Imported) {
        warn!(%err, "could not record the Herdr import decision");
    }
    info!(?plan, report = ?report.lines, "imported from herdr");
    report
}

/// Parses the `--apply` item list of `oasis import-herdr`.
pub fn parse_items(list: &str) -> Result<ImportPlan, String> {
    let mut plan = ImportPlan::default();
    for name in list
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let item = match name {
            "settings" => ImportItem::Settings,
            "remotes" | "machines" => ImportItem::Remotes,
            "windows" => ImportItem::Windows,
            "server" => ImportItem::Server,
            other => {
                return Err(format!(
                    "unknown import item {other:?}; use settings, remotes, windows or server"
                ))
            }
        };
        plan.set(item, true);
    }
    if plan.is_empty() {
        return Err("choose at least one of settings, remotes, windows, server".into());
    }
    Ok(plan)
}

fn socket_accepts(path: &Path) -> bool {
    crate::ipc::connect_local_stream(path).is_ok()
}

fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

/// Stops the first-launch oasis server so it cannot save over the import.
fn stop_oasis_server(sockets: &OasisSockets) {
    if !socket_accepts(&sockets.api) {
        return;
    }
    let client = crate::api::client::ApiClient::for_target(
        crate::api::client::ConnectionTarget::SocketPath(sockets.api.clone()),
    );
    let request = crate::api::schema::Request {
        id: "import:server-stop".into(),
        method: crate::api::schema::Method::ServerStop(Default::default()),
    };
    if let Err(err) = client.request_value_with_timeout(&request, SOCKET_WAIT) {
        warn!(%err, "could not stop the oasis server before importing");
    }
    if !wait_until(SOCKET_WAIT, || {
        !socket_accepts(&sockets.api) && !socket_accepts(&sockets.client)
    }) {
        warn!("oasis server still accepting connections after stop");
    }
}

/// The handoff import executable: oasis, with Oasis's socket and config
/// paths, whatever environment Herdr's server passes down.
pub(crate) fn takeover_shim(oasis_exe: &Path, sockets: &OasisSockets) -> String {
    let quote = |value: &str| format!("'{}'", value.replace('\'', r"'\''"));
    let mut script = String::from(
        "#!/bin/sh\n# Written by oasis: Herdr's server runs this to hand its panes to oasis.\n",
    );
    script.push_str(&format!(
        "export {}={}\n",
        crate::api::SOCKET_PATH_ENV_VAR,
        quote(&sockets.api.to_string_lossy())
    ));
    for name in [
        crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR,
        crate::session::SESSION_ENV_VAR,
    ] {
        script.push_str(&format!("unset {name}\n"));
    }
    for name in [
        crate::config::CONFIG_PATH_ENV_VAR,
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
    ] {
        match std::env::var(name) {
            Ok(value) => script.push_str(&format!("export {name}={}\n", quote(&value))),
            Err(_) => script.push_str(&format!("unset {name}\n")),
        }
    }
    script.push_str(&format!(
        "exec {} \"$@\"\n",
        quote(&oasis_exe.to_string_lossy())
    ));
    script
}

#[cfg(unix)]
fn take_over_server(dirs: &ImportDirs, sockets: &OasisSockets) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let herdr_api = dirs.herdr_api_socket();
    let herdr_client = dirs.herdr_client_socket();
    if !server_availability(dirs, probe_server(&herdr_api)).is_available() {
        return Err(io::Error::other("Herdr's server can't hand off to oasis"));
    }
    let oasis_exe = std::env::current_exe()?;
    std::fs::create_dir_all(&dirs.oasis_state)?;
    let shim = dirs.oasis_state.join(TAKEOVER_SHIM);
    std::fs::write(&shim, takeover_shim(&oasis_exe, sockets))?;
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700))?;

    let params: crate::api::schema::ServerLiveHandoffParams =
        serde_json::from_value(serde_json::json!({
            "import_exe": shim.to_string_lossy(),
            "expected_protocol": crate::protocol::PROTOCOL_VERSION,
        }))
        .map_err(io::Error::other)?;
    let request = crate::api::schema::Request {
        id: "import:herdr-handoff".into(),
        method: crate::api::schema::Method::ServerLiveHandoff(params),
    };
    let client = crate::api::client::ApiClient::for_target(
        crate::api::client::ConnectionTarget::SocketPath(herdr_api.clone()),
    );
    let response = client
        .request_value_with_timeout(&request, HANDOFF_TIMEOUT)
        .map_err(|err| io::Error::other(err.to_string()))?;
    if let Some(error) = response.get("error") {
        return Err(io::Error::other(
            error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("live handoff failed")
                .to_owned(),
        ));
    }
    if !wait_until(SOCKET_WAIT, || socket_accepts(&sockets.client)) {
        return Err(io::Error::other(
            "the oasis server did not come up after the handoff",
        ));
    }
    // Herdr's server exits once the handoff commits; give it a moment.
    wait_until(SOCKET_WAIT, || {
        !socket_accepts(&herdr_api) && !socket_accepts(&herdr_client)
    });
    for (herdr, oasis) in [(&herdr_api, &sockets.api), (&herdr_client, &sockets.client)] {
        link_socket(herdr, oasis)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn take_over_server(_dirs: &ImportDirs, _sockets: &OasisSockets) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "taking over Herdr's server needs Linux or macOS",
    ))
}

/// Points a Herdr socket path at the oasis socket now serving it, so panes,
/// hooks and remote bridges using Herdr's path reach the oasis server. A
/// path some live server still answers on is left alone.
#[cfg(unix)]
pub(crate) fn link_socket(herdr: &Path, oasis: &Path) -> io::Result<()> {
    if std::fs::read_link(herdr).is_ok_and(|target| target == oasis) {
        return Ok(());
    }
    if herdr.exists() && socket_accepts(herdr) {
        return Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("{} is still served by another server", herdr.display()),
        ));
    }
    match std::fs::remove_file(herdr) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    if let Some(parent) = herdr.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::os::unix::fs::symlink(oasis, herdr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oasis-import-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn dirs(root: &Path) -> ImportDirs {
        ImportDirs {
            oasis_config: root.join("config/oasis"),
            oasis_state: root.join("state/oasis"),
            herdr_config: root.join("config/herdr"),
            herdr_state: root.join("state/herdr"),
        }
    }

    fn write(path: PathBuf, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// A Herdr install with every importable item plus its runtime files.
    fn full_install(root: &Path) -> ImportDirs {
        let dirs = dirs(root);
        write(
            dirs.herdr_config.join("config.toml"),
            "[ui]\naccent = \"blue\"\n",
        );
        write(dirs.herdr_config.join("agent-detection/claude.toml"), "x");
        write(dirs.herdr_config.join("session.json"), "{\"version\":1}");
        write(dirs.herdr_config.join("herdr.sock"), "");
        write(dirs.herdr_config.join("herdr-server.log"), "log");
        write(dirs.herdr_state.join("client/endpoints.json"), "[]");
        write(
            dirs.herdr_state.join("client/ssh-metadata/build-box.json"),
            "{}",
        );
        write(dirs.herdr_state.join("client-shell/preferences.json"), "{}");
        dirs
    }

    #[test]
    fn no_herdr_install_offers_nothing() {
        let root = scratch_dir("none");
        assert_eq!(detect(&dirs(&root), |_| ServerProbe::NotRunning), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn detects_each_item_and_defaults_the_server_off() {
        let root = scratch_dir("detect");
        let dirs = full_install(&root);
        let install = detect(&dirs, |_| ServerProbe::NotRunning).expect("install offered");
        assert!(install.settings.is_available());
        assert!(install.remotes.is_available());
        assert!(install.windows.is_available());
        assert_eq!(
            install.server,
            Availability::Unavailable("Herdr's server isn't running".into())
        );
        let plan = install.default_plan();
        assert!(plan.settings && plan.remotes && plan.windows && !plan.server);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_items_are_unavailable_with_a_reason() {
        let root = scratch_dir("partial");
        let dirs = dirs(&root);
        write(dirs.herdr_config.join("config.toml"), "");
        let install = detect(&dirs, |_| ServerProbe::NotRunning).expect("settings offered");
        assert!(install.settings.is_available());
        assert!(!install.remotes.is_available());
        assert!(!install.windows.is_available());
        assert!(!install.default_plan().remotes);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_recorded_decision_stops_the_offer_until_forgotten() {
        let root = scratch_dir("marker");
        let dirs = full_install(&root);
        record_decision(&dirs.oasis_state, Decision::Skipped).unwrap();
        assert_eq!(decision(&dirs.oasis_state), Some(Decision::Skipped));
        assert_eq!(detect(&dirs, |_| ServerProbe::NotRunning), None);
        forget_decision(&dirs.oasis_state).unwrap();
        forget_decision(&dirs.oasis_state).unwrap();
        assert!(detect(&dirs, |_| ServerProbe::NotRunning).is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn server_takeover_needs_a_compatible_running_server() {
        let root = scratch_dir("server");
        let dirs = dirs(&root);
        let running = |live_handoff, protocol| ServerProbe::Running {
            version: "0.9.4".into(),
            protocol,
            live_handoff,
        };
        let ours = crate::protocol::PROTOCOL_VERSION;
        if cfg!(unix) {
            assert_eq!(
                server_availability(&dirs, running(true, ours)),
                Availability::Available
            );
        }
        assert!(!server_availability(&dirs, running(false, ours)).is_available());
        assert!(!server_availability(&dirs, running(true, ours + 1)).is_available());
        assert!(!server_availability(&dirs, ServerProbe::NotRunning).is_available());
        #[cfg(unix)]
        {
            std::fs::create_dir_all(&dirs.herdr_config).unwrap();
            std::os::unix::fs::symlink(
                dirs.oasis_config.join("herdr.sock"),
                dirs.herdr_config.join("herdr.sock"),
            )
            .unwrap();
            assert_eq!(
                server_availability(&dirs, running(true, ours)),
                Availability::Unavailable("Herdr's sockets already lead to oasis".into()),
                "after a takeover Herdr's path reaches oasis itself"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn imports_chosen_items_but_never_runtime_files() {
        let root = scratch_dir("files");
        let dirs = full_install(&root);
        write(
            dirs.oasis_config.join("config.toml"),
            "onboarding = false\n",
        );
        let plan = ImportPlan {
            settings: true,
            remotes: true,
            windows: true,
            server: false,
        };
        let copied = import_files(&dirs, plan).unwrap();
        assert!(copied.contains(&"config.toml".to_owned()));
        assert!(dirs
            .oasis_config
            .join("agent-detection/claude.toml")
            .is_file());
        assert_eq!(
            std::fs::read_to_string(dirs.oasis_config.join("config.toml.before-herdr-import"))
                .unwrap(),
            "onboarding = false\n"
        );
        assert!(dirs
            .oasis_state
            .join("client/ssh-metadata/build-box.json")
            .is_file());
        assert!(dirs
            .oasis_state
            .join("client-shell/preferences.json")
            .is_file());
        assert!(dirs.oasis_config.join("session.json").is_file());
        assert!(!dirs.oasis_config.join("herdr.sock").exists());
        assert!(!dirs.oasis_config.join("herdr-server.log").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unchosen_items_are_left_alone() {
        let root = scratch_dir("unchosen");
        let dirs = full_install(&root);
        let plan = ImportPlan {
            remotes: true,
            ..ImportPlan::default()
        };
        import_files(&dirs, plan).unwrap();
        assert!(!dirs.oasis_config.join("config.toml").exists());
        assert!(!dirs.oasis_config.join("session.json").exists());
        assert!(dirs.oasis_state.join("client/endpoints.json").is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_server_takeover_brings_the_windows_itself() {
        let root = scratch_dir("takeover-windows");
        let dirs = full_install(&root);
        let plan = ImportPlan {
            windows: true,
            server: true,
            ..ImportPlan::default()
        };
        import_files(&dirs, plan).unwrap();
        assert!(
            !dirs.oasis_config.join("session.json").exists(),
            "the handoff carries the live windows; a copied layout would duplicate them"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_takeover_shim_pins_oasis_paths_over_herdr_environment() {
        let sockets = OasisSockets {
            api: PathBuf::from("/run/o'asis/herdr.sock"),
            client: PathBuf::from("/run/oasis/herdr-client.sock"),
        };
        let shim = takeover_shim(Path::new("/usr/bin/oasis"), &sockets);
        assert!(shim.starts_with("#!/bin/sh\n"));
        assert!(shim.contains("export HERDR_SOCKET_PATH='/run/o'\\''asis/herdr.sock'\n"));
        assert!(shim.contains("unset HERDR_CLIENT_SOCKET_PATH\n"));
        assert!(shim.contains("unset HERDR_SESSION\n"));
        assert!(shim.ends_with("exec '/usr/bin/oasis' \"$@\"\n"));
    }

    #[cfg(unix)]
    #[test]
    fn herdr_socket_paths_become_links_to_oasis() {
        let root = scratch_dir("link");
        let herdr = root.join("herdr/herdr.sock");
        let oasis = root.join("oasis/herdr.sock");
        write(herdr.clone(), "");
        link_socket(&herdr, &oasis).unwrap();
        assert_eq!(std::fs::read_link(&herdr).unwrap(), oasis);
        link_socket(&herdr, &oasis).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn a_live_herdr_socket_is_never_replaced() {
        let root = scratch_dir("live");
        let herdr = root.join("herdr.sock");
        let _listener = crate::ipc::bind_private_local_listener(&herdr).unwrap();
        let error = link_socket(&herdr, &root.join("oasis.sock")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
        assert!(std::fs::read_link(&herdr).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn apply_lists_name_the_items() {
        let plan = parse_items("settings, machines,server").unwrap();
        assert!(plan.settings && plan.remotes && plan.server && !plan.windows);
        assert!(parse_items("").is_err());
        assert!(parse_items("sessions")
            .unwrap_err()
            .contains("unknown import item"));
    }

    #[test]
    fn pending_import_is_taken_once() {
        let root = scratch_dir("pending");
        let install = detect(&full_install(&root), |_| ServerProbe::NotRunning).unwrap();
        set_pending(install.clone(), install.default_plan());
        assert_eq!(
            take_pending().map(|(_, plan)| plan),
            Some(install.default_plan())
        );
        assert_eq!(take_pending(), None);
        let _ = std::fs::remove_dir_all(root);
    }
}
