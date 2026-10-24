//! Web pages open in the user's own default browser, in a new window, so their profile,
//! logins and password manager come with it. On Hyprland that window is kept as a panel
//! beside clawmux that one key shows and hides; elsewhere it is an ordinary window.

use std::cell::RefCell;
use std::io;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;

/// The command that opens `url` in a new window of the default browser: the browser's
/// standard "new-window" desktop action when it has one, otherwise `xdg-open`.
pub fn new_window_command(url: &str) -> Vec<String> {
    default_browser_new_window()
        .map(|mut argv| {
            argv.push(url.to_owned());
            argv
        })
        .unwrap_or_else(|| vec!["xdg-open".into(), url.to_owned()])
}

/// The default `https` handler's "new-window" action, as argv without field codes.
fn default_browser_new_window() -> Option<Vec<String>> {
    let app = gio::AppInfo::default_for_type("x-scheme-handler/https", false)?;
    let desktop = app.downcast::<gio_unix::DesktopAppInfo>().ok()?;
    let path = desktop.filename()?;
    let file = glib::KeyFile::new();
    file.load_from_file(&path, glib::KeyFileFlags::NONE).ok()?;
    let action = desktop
        .list_actions()
        .into_iter()
        .find(|action| is_new_window_action(action))?;
    let exec = file
        .string(&format!("Desktop Action {action}"), "Exec")
        .ok()?;
    let argv = ensure_new_window_flag(exec_argv(&exec));
    (!argv.is_empty()).then_some(argv)
}

/// Chromium-family new-window actions are the bare browser command, which turns a URL
/// into a tab in an existing window; `--new-window` (understood by Chromium, Firefox
/// and GNOME Web alike) makes it a window.
fn ensure_new_window_flag(mut argv: Vec<String>) -> Vec<String> {
    if !argv.is_empty()
        && !argv
            .iter()
            .any(|arg| arg == "--new-window" || arg == "-new-window")
    {
        argv.push("--new-window".into());
    }
    argv
}

fn is_new_window_action(action: &str) -> bool {
    let normalized: String = action
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    normalized == "newwindow"
}

/// Splits a desktop-entry Exec line and drops its `%u`-style field codes.
fn exec_argv(exec: &str) -> Vec<String> {
    let Ok(args) = glib::shell_parse_argv(exec) else {
        return Vec::new();
    };
    args.into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .filter(|arg| !(arg.len() == 2 && arg.starts_with('%')))
        .map(|arg| arg.replace("%%", "%"))
        .collect()
}

/// The browser window kept beside clawmux on Hyprland.
#[derive(Default)]
pub struct Panel {
    /// Hyprland address of the browser window, once it has appeared.
    address: RefCell<Option<String>>,
}

const HIDDEN: &str = "special:clawmux-web";

impl Panel {
    /// Opens `url` in a new window of the default browser. On Hyprland the new window
    /// becomes the panel; it opens on clawmux's workspace, where Hyprland tiles it beside
    /// clawmux.
    pub fn open(self: &std::rc::Rc<Self>, url: &str) -> io::Result<()> {
        let hyprland = hyprland();
        let before = if hyprland {
            client_addresses()
        } else {
            Vec::new()
        };
        spawn_detached(&new_window_command(url))?;
        if !hyprland {
            return Ok(());
        }
        // Find the one new window off the GTK thread; this waits only while it appears.
        let (found, receive) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
                if let Some(address) = client_addresses()
                    .into_iter()
                    .find(|address| !before.contains(address))
                {
                    let _ = found.send_blocking(address);
                    return;
                }
            }
        });
        let panel = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(address) = receive.recv().await {
                panel.address.replace(Some(address));
            }
        });
        Ok(())
    }

    /// Shows the panel beside clawmux, or puts it away. Returns false when there is no
    /// panel to toggle (none opened yet, closed by the user, or not on Hyprland).
    pub fn toggle(&self) -> bool {
        let Some(address) = self.address.borrow().clone() else {
            return false;
        };
        let Some(client) = client(&address) else {
            self.address.replace(None);
            return false;
        };
        let hidden = client.workspace.starts_with("special:");
        if hidden {
            let Some(workspace) = active_workspace() else {
                return false;
            };
            hyprctl(&[
                "dispatch",
                "movetoworkspacesilent",
                &format!("{workspace},address:{address}"),
            ]);
            hyprctl(&["dispatch", "focuswindow", &format!("address:{address}")]);
        } else {
            hyprctl(&[
                "dispatch",
                "movetoworkspacesilent",
                &format!("{HIDDEN},address:{address}"),
            ]);
        }
        true
    }
}

fn hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

fn spawn_detached(argv: &[String]) -> io::Result<()> {
    use std::os::unix::process::CommandExt as _;
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no browser command"))?;
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

fn hyprctl(args: &[&str]) -> Option<String> {
    let output = Command::new("hyprctl").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

struct Client {
    workspace: String,
}

fn clients() -> Vec<(String, Client)> {
    let Some(json) = hyprctl(&["-j", "clients"]) else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(list)) = serde_json::from_str(&json) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|client| {
            let address = client.get("address")?.as_str()?.to_owned();
            let workspace = client.get("workspace")?.get("name")?.as_str()?.to_owned();
            Some((address, Client { workspace }))
        })
        .collect()
}

fn client_addresses() -> Vec<String> {
    clients().into_iter().map(|(address, _)| address).collect()
}

fn client(address: &str) -> Option<Client> {
    clients()
        .into_iter()
        .find(|(candidate, _)| candidate == address)
        .map(|(_, client)| client)
}

/// The workspace clawmux is on, as a dispatcher target.
fn active_workspace() -> Option<String> {
    let json = hyprctl(&["-j", "activeworkspace"])?;
    let value: serde_json::Value = serde_json::from_str(&json).ok()?;
    let id = value.get("id")?.as_i64()?;
    Some(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_lines_lose_field_codes_and_keep_quoted_args() {
        assert_eq!(
            exec_argv("/usr/bin/google-chrome-stable --new-window %U"),
            vec!["/usr/bin/google-chrome-stable", "--new-window"]
        );
        assert_eq!(
            exec_argv("firefox --new-window %u"),
            vec!["firefox", "--new-window"]
        );
        assert_eq!(
            exec_argv("\"/opt/My Browser/browser\" --new-window"),
            vec!["/opt/My Browser/browser", "--new-window"]
        );
    }

    #[test]
    fn bare_new_window_actions_get_the_new_window_flag() {
        assert_eq!(
            ensure_new_window_flag(vec!["/usr/bin/google-chrome-stable".into()]),
            vec!["/usr/bin/google-chrome-stable", "--new-window"]
        );
        assert_eq!(
            ensure_new_window_flag(vec!["firefox".into(), "--new-window".into()]),
            vec!["firefox", "--new-window"]
        );
    }

    #[test]
    fn new_window_actions_are_recognized_by_their_common_names() {
        for name in ["new-window", "NewWindow", "new_window", "new-Window"] {
            assert!(is_new_window_action(name), "{name}");
        }
        assert!(!is_new_window_action("new-private-window"));
        assert!(!is_new_window_action("incognito"));
    }
}
