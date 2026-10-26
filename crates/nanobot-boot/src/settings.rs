//! User settings: flat dotted-key JSON persistence for user-level harness
//! state (the theme name, product toggles).
//!
//! The shape follows workbench-rs's settings service (its production
//! hardening batch): a `BTreeMap<String, Value>` behind dotted key strings —
//! `ui.theme`, `plugin.<id>.enabled` — written through on every change. The
//! dot is part of the key, not a nesting operator; there is no schema and no
//! defaults table.
//!
//! Settings are **user-scoped, not project-scoped**: they live under
//! [`user_config_dir`] keyed by the harness id (`~/.config/<id>` on Unix,
//! `%APPDATA%\<id>` on Windows), so the theme survives a project switch.
//! Loading is never fatal — a missing file is empty settings, a corrupt file
//! resets with a message for the assembly notes. The file is only ever
//! written by an explicit change ([`Settings::set`] / [`Settings::remove`]):
//! an assembly that changed nothing writes nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

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

/// The user settings store. Shared behind a mutex by the harness; owned by
/// whoever calls `set` (on a UI, that is the UI thread).
#[derive(Debug, Default)]
pub struct Settings {
    values: BTreeMap<String, Value>,
    path: Option<PathBuf>,
}

impl Settings {
    /// Loads the settings from `path`. Missing file: empty settings. Corrupt
    /// file: empty settings plus a message for the caller to surface. Never
    /// an `Err` — settings cannot block an assembly.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(_) => return (Self::with_path(path), None),
        };
        match serde_json::from_str::<BTreeMap<String, Value>>(&text) {
            Ok(values) => {
                let mut settings = Self::with_path(path);
                settings.values = values;
                (settings, None)
            }
            Err(error) => (
                Self::with_path(path),
                Some(format!(
                    "settings file {} could not be parsed ({error}); starting empty",
                    path.display()
                )),
            ),
        }
    }

    /// Settings without a persistence path (tests, in-memory shells).
    pub fn in_memory() -> Self {
        Self::default()
    }

    fn with_path(path: &Path) -> Self {
        Self {
            values: BTreeMap::new(),
            path: Some(path.to_path_buf()),
        }
    }

    /// The file these settings persist to, when they persist at all.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    pub fn get_str(&self, key: &str, default: &str) -> String {
        self.get(key)
            .and_then(Value::as_str)
            .unwrap_or(default)
            .to_string()
    }

    /// Sets `key` and writes the whole file through immediately (write-through:
    /// a crash after a settings change loses nothing). Without a path the
    /// change lives in memory only.
    pub fn set(&mut self, key: &str, value: Value) -> Result<(), String> {
        self.values.insert(key.to_string(), value);
        self.save()
    }

    /// Removes `key` and writes through, mirroring [`set`](Self::set).
    pub fn remove(&mut self, key: &str) -> Result<(), String> {
        self.values.remove(key);
        self.save()
    }

    /// Writes the full file. An error is a message, never a panic — a
    /// settings failure must not take a harness down.
    pub fn save(&mut self) -> Result<(), String> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let json = serde_json::to_string_pretty(&self.values).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(path, json).map_err(|e| format!("write settings {}: {e}", path.display()))
    }
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
        let (settings, error) = Settings::load(&dir.join("settings.json"));
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

        let (settings, error) = Settings::load(&file);
        assert!(error.is_some(), "the reset is reported, not silent");
        assert!(settings.get("ui.theme").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_writes_through_and_round_trips() {
        let dir = scratch("roundtrip");
        let file = dir.join("nested").join("settings.json");
        assert!(!file.exists());

        let mut settings = Settings::load(&file).0;
        settings.set("ui.theme", Value::String("light".into())).expect("set");
        assert!(file.is_file(), "set writes the file immediately");

        let reloaded = Settings::load(&file).0;
        assert_eq!(reloaded.get_str("ui.theme", "dark"), "light");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_str_falls_back_to_its_default() {
        let mut settings = Settings::in_memory();
        settings.values.insert("ui.theme".into(), Value::String("light".into()));

        assert_eq!(settings.get_str("ui.theme", "dark"), "light");
        assert_eq!(settings.get_str("absent", "dark"), "dark");
    }

    #[test]
    fn in_memory_settings_accept_changes_without_a_file() {
        let mut settings = Settings::in_memory();
        settings.set("k", Value::Bool(true)).expect("memory set");
        assert_eq!(settings.get("k"), Some(&Value::Bool(true)));
    }
}
