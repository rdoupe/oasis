//! One-time import of settings from a Herdr install.
//!
//! Oasis is a fork of Herdr and keeps its own config and state directories. The
//! first run copies the user's Herdr settings so the fork starts where Herdr left
//! off; after that the two installs are independent. Runtime files (sockets,
//! logs, session snapshots) are never copied: they belong to Herdr's servers.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

/// Settings kept under the config directory.
const CONFIG_ENTRIES: &[&str] = &["config.toml", "agent-detection", "plugins", "plugins.json"];
/// Client settings kept under the state directory: saved machines and layout.
const STATE_ENTRIES: &[&str] = &["client", "client-shell"];

pub(super) fn legacy_app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

/// Copies Herdr's settings into fresh Oasis directories. Does nothing once
/// either Oasis directory exists, so it runs at most once per install.
pub fn import_legacy_settings() {
    let config_dir = super::config_dir();
    let state_dir = super::state_dir();
    if config_dir.exists() || state_dir.exists() {
        return;
    }
    let legacy_config = sibling_dir(&config_dir, legacy_app_dir_name());
    let legacy_state = sibling_dir(&state_dir, legacy_app_dir_name());
    let copied = [
        copy_entries(legacy_config.as_deref(), &config_dir, CONFIG_ENTRIES),
        copy_entries(legacy_state.as_deref(), &state_dir, STATE_ENTRIES),
    ]
    .into_iter()
    .sum::<usize>();
    if copied > 0 {
        info!(copied, "imported settings from herdr");
    }
}

fn sibling_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    Some(dir.parent()?.join(name))
}

fn copy_entries(source: Option<&Path>, destination: &Path, entries: &[&str]) -> usize {
    let Some(source) = source.filter(|source| source.is_dir()) else {
        return 0;
    };
    let mut copied = 0;
    for entry in entries {
        let from = source.join(entry);
        let to = destination.join(entry);
        let result = if from.is_dir() {
            crate::plugin_paths::copy_dir_all(&from, &to)
        } else if from.is_file() {
            std::fs::create_dir_all(destination).and_then(|()| std::fs::copy(&from, &to).map(drop))
        } else {
            continue;
        };
        match result {
            Ok(()) => copied += 1,
            Err(err) => warn!(from = %from.display(), "failed to import herdr setting: {err}"),
        }
    }
    copied
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "oasis-legacy-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copies_settings_but_not_runtime_files() {
        let root = scratch_dir("copy");
        let legacy = root.join("herdr");
        std::fs::create_dir_all(legacy.join("agent-detection")).unwrap();
        std::fs::write(legacy.join("config.toml"), "[ui]\n").unwrap();
        std::fs::write(legacy.join("agent-detection/claude.toml"), "x").unwrap();
        std::fs::write(legacy.join("herdr.sock"), "").unwrap();
        std::fs::write(legacy.join("session.json"), "{}").unwrap();

        let destination = root.join("oasis");
        let copied = copy_entries(Some(&legacy), &destination, CONFIG_ENTRIES);

        assert_eq!(copied, 2);
        assert!(destination.join("config.toml").is_file());
        assert!(destination.join("agent-detection/claude.toml").is_file());
        assert!(!destination.join("herdr.sock").exists());
        assert!(!destination.join("session.json").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn missing_legacy_dir_copies_nothing() {
        let root = scratch_dir("missing");
        let destination = root.join("oasis");
        let copied = copy_entries(Some(&root.join("herdr")), &destination, CONFIG_ENTRIES);
        assert_eq!(copied, 0);
        assert!(!destination.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
