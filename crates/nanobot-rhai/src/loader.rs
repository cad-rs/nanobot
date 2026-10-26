//! Plugin discovery and loading (design 4.8).
//!
//! A plugin directory holds `plugin.toml` and its Rhai scripts. Loading is
//! **isolated**: a bad manifest, a missing script, a compile error or a
//! duplicate id lands in [`LoadedPlugins::plugins_rejected`] (or a note) and
//! never interrupts the assembly of the rest — the same discipline the harness
//! applies to skill fibers.

use std::path::{Path, PathBuf};

use crate::host::{build_load_engine, Collected, RhaiProgram, SharedCollected};
use crate::manifest::{parse, split_entry_point, validate, PluginManifest};

/// A dock panel a plugin declared.
#[derive(Debug, Clone)]
pub struct PanelRegistration {
    pub plugin: String,
    pub id: String,
    pub title: String,
    pub script: PathBuf,
    pub area: String,
}

/// A central view a plugin declared.
#[derive(Debug, Clone)]
pub struct ViewRegistration {
    pub plugin: String,
    pub id: String,
    pub title: String,
    pub script: PathBuf,
}

/// A command a plugin declared. `source` is the entry script it is defined in.
#[derive(Debug, Clone)]
pub struct CommandRegistration {
    pub plugin: String,
    pub id: String,
    pub title: String,
    pub callback: String,
    pub background: bool,
    pub source: RhaiProgram,
}

/// A toolbar item a plugin declared.
#[derive(Debug, Clone)]
pub struct ToolbarItemRegistration {
    pub plugin: String,
    pub group: String,
    pub command: String,
    pub label: Option<String>,
}

/// Everything the loaded plugins contribute, plus what was rejected and why.
#[derive(Debug, Default)]
pub struct LoadedPlugins {
    pub plugins_loaded: Vec<String>,
    pub panels: Vec<PanelRegistration>,
    pub views: Vec<ViewRegistration>,
    pub commands: Vec<CommandRegistration>,
    pub toolbar: Vec<ToolbarItemRegistration>,
    pub notes: Vec<String>,
    /// `(plugin directory, reason)` for every plugin that did not load.
    pub plugins_rejected: Vec<(String, String)>,
}

/// Every plugin directory under `dirs`: immediate subdirectories that hold a
/// `plugin.toml`, sorted for deterministic load order.
pub fn discover(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.join("plugin.toml").is_file() {
                found.push(path);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Loads one plugin directory. `Err(reason)` means the plugin is rejected.
pub fn load_one(dir: &Path) -> Result<(PluginManifest, RhaiProgram, Collected), String> {
    let manifest_path = dir.join("plugin.toml");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
    let manifest = parse(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;
    validate(&manifest).map_err(|e| format!("{}: {e}", manifest_path.display()))?;

    let (entry_file, entry_fn) = split_entry_point(&manifest.plugin.entry_point);
    let entry_path = dir.join(&entry_file);
    let source = std::fs::read_to_string(&entry_path)
        .map_err(|e| format!("cannot read {}: {e}", entry_path.display()))?;

    let collected: SharedCollected = std::sync::Arc::new(std::sync::Mutex::new(Collected::default()));
    let program: RhaiProgram = std::sync::Arc::new(source);
    {
        let engine = build_load_engine(dir.to_path_buf(), collected.clone());
        let ast = engine
            .compile(program.as_str())
            .map_err(|e| format!("{}: {e}", entry_path.display()))?;
        let mut scope = rhai::Scope::new();
        engine
            .call_fn::<()>(&mut scope, &ast, &entry_fn, ())
            .map_err(|e| format!("{}: entry point `{entry_fn}` failed: {e}", entry_path.display()))?;
        // The engine's injected globals hold clones of the collection bucket, so
        // it must be dropped before the bucket can be unwrapped.
    }

    let collected = std::sync::Arc::try_unwrap(collected)
        .map_err(|_| "plugin engine still alive".to_string())?
        .into_inner()
        .map_err(|_| "collected bucket poisoned".to_string())?;
    Ok((manifest, program, collected))
}

/// The license gate a plugin must pass before it loads: given the validated
/// manifest and the plugin directory, `Err(reason)` rejects it.
///
/// A plugin that ships no license is open and passes any gate that allows
/// it; a plugin that ships one must verify. The host supplies the gate —
/// typically [`nanobot_auth::check_plugin_license`] over the vendor's
/// verifying keys — so the plugin host stays free of keys and crypto.
pub type PluginGate<'a> = &'a dyn Fn(&PluginManifest, &Path) -> Result<(), String>;

/// Loads every plugin under `dirs` without license checking (an open plugin
/// set). See [`load_all_with`].
pub fn load_all(dirs: &[PathBuf]) -> LoadedPlugins {
    load_all_with(dirs, &|_, _| Ok(()))
}

/// Loads every plugin under `dirs`, draining each entry point's declarations
/// into one result. Duplicate ids are rejected individually so one plugin
/// cannot silently shadow another's panel or command.
///
/// `gate` runs after the manifest validates and before the entry point
/// compiles; a plugin it rejects lands in `plugins_rejected` with the reason,
/// next to the other load failures.
pub fn load_all_with(dirs: &[PathBuf], gate: PluginGate<'_>) -> LoadedPlugins {
    let mut loaded = LoadedPlugins::default();
    let mut plugin_ids = Vec::new();
    let mut panel_ids = Vec::new();
    let mut view_ids = Vec::new();
    let mut command_ids = Vec::new();

    for dir in discover(dirs) {
        let name = dir.display().to_string();
        let (manifest, program, collected) = match load_one(&dir) {
            Ok(loaded) => loaded,
            Err(reason) => {
                loaded.plugins_rejected.push((name, reason));
                continue;
            }
        };
        if let Err(reason) = gate(&manifest, &dir) {
            loaded.plugins_rejected.push((name, format!("license: {reason}")));
            continue;
        }
        if plugin_ids.contains(&manifest.plugin.id) {
            loaded.plugins_rejected.push((
                name,
                format!("duplicate plugin id `{}`", manifest.plugin.id),
            ));
            continue;
        }
        plugin_ids.push(manifest.plugin.id.clone());

        for command in collected.commands {
            if command_ids.contains(&command.id) {
                loaded.notes.push(format!(
                    "command `{}` from `{}` duplicates an existing id; ignored",
                    command.id, manifest.plugin.id
                ));
                continue;
            }
            command_ids.push(command.id.clone());
            loaded.commands.push(CommandRegistration {
                plugin: manifest.plugin.id.clone(),
                id: command.id,
                title: command.title,
                callback: command.callback,
                background: command.background,
                source: program.clone(),
            });
        }
        for (id, command, label) in collected.toolbar {
            loaded.toolbar.push(ToolbarItemRegistration {
                plugin: manifest.plugin.id.clone(),
                group: id,
                command,
                label,
            });
        }
        for (id, title, script, area) in collected.panels {
            if panel_ids.contains(&id) {
                loaded.notes.push(format!(
                    "panel `{id}` from `{}` duplicates an existing id; ignored",
                    manifest.plugin.id
                ));
                continue;
            }
            panel_ids.push(id.clone());
            loaded.panels.push(PanelRegistration {
                plugin: manifest.plugin.id.clone(),
                id,
                title,
                script,
                area,
            });
        }
        for (id, title, script) in collected.views {
            if view_ids.contains(&id) {
                loaded.notes.push(format!(
                    "view `{id}` from `{}` duplicates an existing id; ignored",
                    manifest.plugin.id
                ));
                continue;
            }
            view_ids.push(id.clone());
            loaded.views.push(ViewRegistration {
                plugin: manifest.plugin.id.clone(),
                id,
                title,
                script,
            });
        }
        loaded.notes.extend(collected.notes);
        loaded.plugins_loaded.push(manifest.plugin.id);
    }

    loaded
}
