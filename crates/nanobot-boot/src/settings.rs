//! User settings: flat dotted-key JSON persistence for user-level harness
//! state (the theme name, product toggles).
//!
//! The store lives in `product_kit` (shared with the workbench framework);
//! this module keeps the harness's file convention ([`user_config_dir`],
//! [`SETTINGS_FILE`]) and the file-backed store.
//!
//! Settings are **user-scoped, not project-scoped**: they live under
//! [`user_config_dir`] keyed by the harness id (`~/.config/<id>` on Unix,
//! `%APPDATA%\<id>` on Windows), so the theme survives a project switch.
//! Loading is never fatal — a missing file is empty settings, a corrupt file
//! resets with a message for the assembly notes. The file is only ever
//! written by an explicit change ([`Settings::set`] / [`Settings::remove`]):
//! an assembly that changed nothing writes nothing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use product_kit::Settings;

/// The conventional file name under the user config directory.
pub const SETTINGS_FILE: &str = "settings.json";

/// The user-level configuration directory for a harness identity. Every
/// product gets its own subtree, keyed by the `harness.toml` id, so two
/// harnesses on one machine never share a settings file.
pub fn user_config_dir(harness_id: &str) -> PathBuf {
    #[cfg(target_arch = "wasm32")]
    {
        // The web has no OS config directory; the file operations fail soft
        // there and the host (storage APIs) owns persistence.
        PathBuf::from("nanobot").join(harness_id)
    }
    #[cfg(all(not(target_arch = "wasm32"), target_os = "windows"))]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(harness_id)
    }
    #[cfg(all(not(target_arch = "wasm32"), not(target_os = "windows")))]
    {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(".config"))
            .unwrap_or_else(std::env::temp_dir)
            .join(harness_id)
    }
}

/// The settings document as a file under the user config directory.
struct FileStore(PathBuf);

impl product_kit::SettingsStore for FileStore {
    fn read(&self) -> Option<Vec<u8>> {
        std::fs::read(&self.0).ok()
    }

    fn write(&self, bytes: &[u8]) -> Result<(), String> {
        if let Some(dir) = self.0.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        crate::write_whole(&self.0, bytes)
            .map_err(|e| format!("write settings {}: {e}", self.0.display()))
    }

    fn describe(&self) -> Option<String> {
        Some(self.0.display().to_string())
    }
}

/// Loads the settings from `path`. Missing file: empty settings. Corrupt
/// file: empty settings plus a message for the caller to surface. Never
/// an `Err` — settings cannot block an assembly.
pub fn load_settings(path: &Path) -> (Settings, Option<String>) {
    Settings::load(Arc::new(FileStore(path.to_path_buf())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nb-settings-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_missing_file_loads_empty_without_a_message() {
        let dir = scratch("missing");
        let (settings, error) = load_settings(&dir.join("settings.json"));
        assert!(error.is_none(), "a missing settings file is not an event");
        assert!(settings.get("ui.theme").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_file_resets_with_a_message() {
        let dir = scratch("corrupt");
        let file = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "{ not json").unwrap();

        let (settings, error) = load_settings(&file);
        assert!(error.is_some(), "the reset is reported, not silent");
        assert!(settings.get("ui.theme").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_writes_through_and_round_trips() {
        let dir = scratch("roundtrip");
        let file = dir.join("nested").join("settings.json");
        assert!(!file.exists());

        let mut settings = load_settings(&file).0;
        settings.set("ui.theme", serde_json::Value::String("light".into())).expect("set");
        assert!(file.is_file(), "set writes the file immediately");

        let reloaded = load_settings(&file).0;
        assert_eq!(reloaded.get_str("ui.theme", "dark"), "light");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_str_falls_back_to_its_default() {
        let mut settings = Settings::in_memory();
        settings.set("ui.theme", serde_json::Value::String("light".into())).unwrap();
        assert_eq!(settings.get_str("ui.theme", "dark"), "light");
        assert_eq!(settings.get_str("absent", "dark"), "dark");
    }

    #[test]
    fn in_memory_settings_accept_changes_without_a_file() {
        let mut settings = Settings::in_memory();
        settings.set("k", serde_json::Value::Bool(true)).expect("memory set");
        assert_eq!(settings.get("k"), Some(&serde_json::Value::Bool(true)));
    }

    #[test]
    fn the_location_is_reported_for_diagnostics() {
        let dir = scratch("location");
        let file = dir.join("settings.json");
        let settings = load_settings(&file).0;
        assert_eq!(
            settings.location().as_deref(),
            Some(file.display().to_string().as_str())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
