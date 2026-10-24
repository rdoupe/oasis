//! clawmux: a native GTK4 window onto the oasis terminal workspace server.
//!
//! Running it again while it is open toggles the window, so one key binding brings it up
//! and puts it away instantly; the process and its connection stay alive in between.

mod app;
mod browser;
mod conn;
mod grid;
mod keys;
mod logos;
mod sidebar;
mod termview;
mod theme;

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};

const APP_ID: &str = "dev.oasis.Clawmux";

/// The path of this program when the running image was replaced on disk by a new build.
fn replaced_binary() -> Option<std::path::PathBuf> {
    let link = std::fs::read_link("/proc/self/exe").ok()?;
    let path = link.to_str()?.strip_suffix(" (deleted)")?;
    let path = std::path::PathBuf::from(path);
    path.is_file().then_some(path)
}

fn main() -> glib::ExitCode {
    let application = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    let state: Rc<RefCell<Option<Rc<app::App>>>> = Rc::new(RefCell::new(None));

    let quit = gio::SimpleAction::new("quit", None);
    let app_ref = application.clone();
    quit.connect_activate(move |_, _| app_ref.quit());
    application.add_action(&quit);
    application.set_accels_for_action("app.quit", &["<Ctrl><Shift>q"]);

    let shared = state.clone();
    application.connect_command_line(move |application, command_line| {
        let args: Vec<String> = command_line
            .arguments()
            .iter()
            .skip(1)
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        if args.iter().any(|arg| arg == "--quit") {
            application.quit();
            return 0.into();
        }
        let mut slot = shared.borrow_mut();
        match slot.as_ref() {
            None => {
                // Keep running while the window is put away.
                std::mem::forget(application.hold());
                let app = app::App::build(application);
                app.show();
                *slot = Some(app);
            }
            // A newer build was installed over the running one: become it, so a launch
            // after an update never brings back the old window.
            Some(_) if replaced_binary().is_some() => {
                if let Some(path) = replaced_binary() {
                    use std::os::unix::process::CommandExt as _;
                    let error = std::process::Command::new(path).arg("--show").exec();
                    eprintln!("clawmux: could not restart onto the new build: {error}");
                }
            }
            Some(app) if args.iter().any(|arg| arg == "--show") => app.show(),
            Some(app) => app.toggle(),
        }
        0.into()
    });
    application.run()
}
