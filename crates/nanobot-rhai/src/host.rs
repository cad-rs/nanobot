//! The Rhai-facing host API (design 4.8).
//!
//! A plugin's entry point is a Rhai function (`register` by default) that calls
//! the injected globals to declare what it contributes:
//!
//! ```rhai
//! fn register() {
//!     register_panel("acme.chart", "温度曲线", "chart.rhai", "right");
//!     register_view("acme.thermal", "热力学视图", "thermal.rhai");
//!     register_command("acme.analyze", "分析", "analyze");
//! }
//! fn analyze(args) { "分析完成" }
//! ```
//!
//! Thread model: rhai's `Engine` and `AST` are **not `Send`**, so a callback
//! holds only the script source ([`RhaiProgram`], an `Arc<String>`) and rebuilds
//! plus compiles the engine on the calling thread. Scripts are small, so the
//! cost is milliseconds. Nothing here caches an engine across calls.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rhai::{Dynamic, EvalAltResult, Map, Scope};

/// Sentinel a cancelled background command returns instead of a value.
pub const CANCEL_SENTINEL: &str = "__nanobot_cancelled__";

/// Dock areas a panel may ask for.
pub const DOCK_AREAS: [&str; 4] = ["left", "right", "bottom", "top"];

/// A command the plugin declared.
#[derive(Debug, Clone)]
pub struct CollectedCommand {
    pub id: String,
    pub title: String,
    pub callback: String,
    pub background: bool,
}

/// Everything one plugin's entry point declared. The loader drains this into
/// the host's registries.
#[derive(Debug, Default)]
pub struct Collected {
    pub commands: Vec<CollectedCommand>,
    /// `(group, command, label)`; a `None` label means icon-only.
    pub toolbar: Vec<(String, String, Option<String>)>,
    /// `(id, title, script path, dock area)`.
    pub panels: Vec<(String, String, PathBuf, String)>,
    /// `(id, title, script path)`.
    pub views: Vec<(String, String, PathBuf)>,
    pub notes: Vec<String>,
}

pub type SharedCollected = Arc<Mutex<Collected>>;

/// A plugin's compiled entry point source. `Send` (unlike the engine).
pub type RhaiProgram = Arc<String>;

/// Arguments handed to a command callback.
#[derive(Debug, Clone, Default)]
pub struct CommandArgs {
    pub values: BTreeMap<String, String>,
    pub value: Option<String>,
}

/// The outcome of a command callback.
#[derive(Debug, Clone)]
pub enum CommandResult {
    Done { note: Option<String> },
    Failed { error: String },
}

impl CommandResult {
    pub fn done() -> Self {
        CommandResult::Done { note: None }
    }

    pub fn done_with(note: impl Into<String>) -> Self {
        CommandResult::Done {
            note: Some(note.into()),
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        CommandResult::Failed {
            error: error.into(),
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, CommandResult::Done { .. })
    }
}

/// Why a background command stopped.
#[derive(Debug, Clone)]
pub enum TaskFailure {
    Cancelled,
    Error(String),
}

/// Progress / cancellation sink the host hands to a background command.
///
/// Implemented by the host (a gpui progress handle, or a test recorder); the
/// script only sees the `Task` handle type.
pub trait ProgressSink: Send + Sync {
    fn report(&self, fraction: Option<f32>, stage: Option<String>);
    fn log(&self, message: String);
    fn is_cancelled(&self) -> bool;
}

/// The `Task` value a background callback receives.
#[derive(Clone)]
struct RhaiTask {
    handle: Arc<dyn ProgressSink>,
}

/// Builds the engine a plugin's entry point runs under. Injected globals only
/// *collect* declarations; nothing is registered with the host here.
pub fn build_load_engine(plugin_dir: PathBuf, collected: SharedCollected) -> rhai::Engine {
    let mut engine = rhai::Engine::new();

    {
        let collected = collected.clone();
        engine.register_fn(
            "register_command",
            move |id: &str, title: &str, callback: &str| {
                collected.lock().expect("collected").commands.push(CollectedCommand {
                    id: id.to_string(),
                    title: title.to_string(),
                    callback: callback.to_string(),
                    background: false,
                });
            },
        );
    }
    {
        let collected = collected.clone();
        engine.register_fn(
            "register_background_command",
            move |id: &str, title: &str, callback: &str| {
                collected.lock().expect("collected").commands.push(CollectedCommand {
                    id: id.to_string(),
                    title: title.to_string(),
                    callback: callback.to_string(),
                    background: true,
                });
            },
        );
    }
    {
        let collected = collected.clone();
        engine.register_fn(
            "add_toolbar_item",
            move |group: &str, command: &str, label: &str| {
                collected
                    .lock()
                    .expect("collected")
                    .toolbar
                    .push((group.to_string(), command.to_string(), Some(label.to_string())));
            },
        );
    }
    {
        let collected = collected.clone();
        engine.register_fn(
            "add_toolbar_item_no_label",
            move |group: &str, command: &str| {
                collected
                    .lock()
                    .expect("collected")
                    .toolbar
                    .push((group.to_string(), command.to_string(), None));
            },
        );
    }
    {
        let plugin_dir = plugin_dir.clone();
        let collected = collected.clone();
        engine.register_fn(
            "register_panel",
            move |id: &str, title: &str, script: &str, area: &str| {
                let area = area.to_ascii_lowercase();
                // An unknown area is ignored rather than fatal: a typo in a
                // plugin must not take the plugin (or the host) down.
                if !DOCK_AREAS.contains(&area.as_str()) {
                    collected.lock().expect("collected").notes.push(format!(
                        "panel `{id}` asked for unknown dock area `{area}`; ignored"
                    ));
                    return;
                }
                collected.lock().expect("collected").panels.push((
                    id.to_string(),
                    title.to_string(),
                    plugin_dir.join(script),
                    area,
                ));
            },
        );
    }
    {
        let plugin_dir = plugin_dir;
        let collected = collected.clone();
        engine.register_fn(
            "register_view",
            move |id: &str, title: &str, script: &str| {
                collected.lock().expect("collected").views.push((
                    id.to_string(),
                    title.to_string(),
                    plugin_dir.join(script),
                ));
            },
        );
    }
    {
        engine.register_fn("log_info", move |message: &str| {
            collected
                .lock()
                .expect("collected")
                .notes
                .push(message.to_string());
        });
    }

    engine
}

/// Builds the engine a command callback runs under: the `Task` handle plus the
/// progress/cancellation methods.
fn build_exec_engine() -> rhai::Engine {
    let mut engine = rhai::Engine::new();
    engine.register_type_with_name::<RhaiTask>("Task");

    engine.register_fn(
        "report",
        |task: &mut RhaiTask, fraction: rhai::FLOAT, stage: &str| {
            task.handle.report(Some(fraction as f32), Some(stage.to_string()));
        },
    );
    engine.register_fn("report_progress", |task: &mut RhaiTask, fraction: rhai::FLOAT| {
        task.handle.report(Some(fraction as f32), None);
    });
    engine.register_fn("set_stage", |task: &mut RhaiTask, stage: &str| {
        task.handle.report(None, Some(stage.to_string()));
    });
    engine.register_fn("log", |task: &mut RhaiTask, message: &str| {
        task.handle.log(message.to_string());
    });
    engine.register_fn("is_cancelled", |task: &mut RhaiTask| task.handle.is_cancelled());
    engine.register_fn(
        "check_cancelled",
        |task: &mut RhaiTask| -> Result<(), Box<EvalAltResult>> {
            if task.handle.is_cancelled() {
                Err(format!("{CANCEL_SENTINEL}: cancellation requested").into())
            } else {
                Ok(())
            }
        },
    );

    engine
}

/// Converts command arguments into the map a callback receives.
pub fn args_to_map(args: &CommandArgs) -> Map {
    let mut map = Map::new();
    for (key, value) in &args.values {
        map.insert(key.as_str().into(), Dynamic::from(value.clone()));
    }
    if let Some(value) = &args.value {
        map.insert("value".into(), Dynamic::from(value.clone()));
    }
    map
}

/// An empty argument map.
pub fn empty_map() -> Map {
    Map::new()
}

/// Runs a plugin's synchronous command callback.
pub fn call_sync_callback(
    program: &RhaiProgram,
    callback: &str,
    args: &CommandArgs,
) -> CommandResult {
    let engine = build_exec_engine();
    let Ok(ast) = engine.compile(program.as_str()) else {
        return CommandResult::failed(format!("script failed to compile: `{program}`"));
    };
    let mut scope = Scope::new();
    let map = args_to_map(args);
    match engine.call_fn::<Dynamic>(&mut scope, &ast, callback, (map,)) {
        Ok(value) if value.is_unit() => CommandResult::done(),
        Ok(value) => match value.into_string() {
            Ok(note) => CommandResult::done_with(note),
            Err(_) => CommandResult::done(),
        },
        Err(error) => CommandResult::failed(format!("script callback `{callback}` failed: {error}")),
    }
}

/// Runs a plugin's background command callback until completion or cancellation.
pub fn run_background_callback(
    program: &RhaiProgram,
    callback: &str,
    handle: Arc<dyn ProgressSink>,
) -> Result<(), TaskFailure> {
    let engine = build_exec_engine();
    let ast = engine
        .compile(program.as_str())
        .map_err(|e| TaskFailure::Error(format!("script failed to compile: {e}")))?;
    let mut scope = Scope::new();
    let task = RhaiTask { handle };
    match engine.call_fn::<Dynamic>(&mut scope, &ast, callback, (task, empty_map())) {
        Ok(_) => Ok(()),
        Err(error) => {
            let message = error.to_string();
            if message.contains(CANCEL_SENTINEL) {
                Err(TaskFailure::Cancelled)
            } else {
                Err(TaskFailure::Error(message))
            }
        }
    }
}
