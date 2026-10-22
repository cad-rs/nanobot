//! nanobot-rhai: the script-plugin host (design 4.8).
//!
//! Plugins are **declarative**: a `plugin.toml` manifest plus Rhai scripts whose
//! entry point registers panels, views, commands and toolbar items. A script
//! panel renders a `gpui-rhai` declarative `UiNode` tree — never arbitrary code
//! — and a plugin never contributes domain capability: that stays a
//! compile-time Rust module (the trust boundary of design-harness §5/§20).
//!
//! ```text
//! plugin.toml  ->  manifest (id / api_version / entry_point / permissions)
//! plugin.rhai  ->  register() calling register_panel / register_view /
//!                   register_command / add_toolbar_item / log_info
//! ```
//!
//! Loading is isolated: a rejected plugin lands in `plugins_rejected` and does
//! not interrupt the assembly of the others.

mod host;
mod loader;
mod manifest;

pub use host::{
    args_to_map, call_sync_callback, empty_map, run_background_callback, build_load_engine,
    CommandArgs, CommandResult, Collected, CollectedCommand, ProgressSink, RhaiProgram,
    SharedCollected, TaskFailure, CANCEL_SENTINEL, DOCK_AREAS,
};
pub use loader::{
    discover, load_all, load_one, CommandRegistration, LoadedPlugins, PanelRegistration,
    ToolbarItemRegistration, ViewRegistration,
};
pub use manifest::{
    parse as parse_manifest, split_entry_point, validate as validate_manifest, Compatibility,
    Permissions, PluginInfo, PluginManifest, HOST_API_VERSION,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A parent directory holding one plugin per call, the shape
    /// [`load_all`] discovers from.
    fn plugin_parent(test: &str) -> PathBuf {
        let parent = std::env::temp_dir().join(format!(
            "nanobot-rhai-test-{test}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&parent);
        std::fs::create_dir_all(&parent).expect("create parent dir");
        parent
    }

    /// Writes one plugin into `parent/<name>` and returns its directory.
    fn write_plugin(
        parent: &Path,
        name: &str,
        manifest: &str,
        entry: &str,
        extra: &[(&str, &str)],
    ) -> PathBuf {
        let dir = parent.join(name);
        std::fs::create_dir_all(&dir).expect("create plugin dir");
        std::fs::write(dir.join("plugin.toml"), manifest).expect("write manifest");
        std::fs::write(dir.join("plugin.rhai"), entry).expect("write entry");
        for (file, content) in extra {
            std::fs::write(dir.join(file), content).expect("write extra script");
        }
        dir
    }

    const MANIFEST: &str = r#"
[plugin]
id = "com.example.demo"
name = "Demo"
version = "0.1.0"
api_version = "1"
entry_point = "plugin.rhai"

[compatibility]
products = ["*"]

[permissions]
document_read = false
network = false
"#;

    const ENTRY: &str = r#"
fn register() {
    register_panel("acme.chart", "温度曲线", "chart.rhai", "right");
    register_view("acme.thermal", "热力学视图", "thermal.rhai");
    register_command("acme.analyze", "分析", "analyze");
    register_background_command("acme.job", "后台任务", "job");
    add_toolbar_item("acme", "acme.analyze", "分析");
    log_info("demo loaded");
}
fn analyze(args) { `分析 ${args.value}` }
fn job(task, args) {
    task.report_progress(0.5);
    task.log("working");
    task.check_cancelled()
}
"#;

    #[test]
    fn a_plugin_declares_its_panels_views_and_commands() {
        let parent = plugin_parent("ok");
        let dir = write_plugin(
            &parent,
            "demo",
            MANIFEST,
            ENTRY,
            &[("chart.rhai", "fn main() { text(\"chart\") }")],
        );
        let loaded = load_all(&[parent.clone()]);
        assert!(loaded.plugins_rejected.is_empty(), "{:?}", loaded.plugins_rejected);
        assert_eq!(loaded.plugins_loaded, vec!["com.example.demo".to_string()]);
        assert_eq!(loaded.panels.len(), 1);
        assert_eq!(loaded.panels[0].id, "acme.chart");
        assert_eq!(loaded.panels[0].area, "right");
        assert_eq!(loaded.panels[0].script, dir.join("chart.rhai"));
        assert_eq!(loaded.views.len(), 1);
        assert_eq!(loaded.views[0].id, "acme.thermal");
        assert_eq!(loaded.commands.len(), 2);
        assert_eq!(loaded.commands[0].id, "acme.analyze");
        assert!(!loaded.commands[0].background);
        assert!(loaded.commands[1].background);
        assert_eq!(loaded.toolbar.len(), 1);
        assert_eq!(loaded.toolbar[0].label.as_deref(), Some("分析"));
        assert!(loaded.notes.iter().any(|n| n.contains("demo loaded")));
    }

    #[test]
    fn a_rejected_plugin_does_not_stop_the_others() {
        let parent = plugin_parent("mixed");
        let bad = write_plugin(&parent, "bad", MANIFEST, ENTRY, &[]);
        std::fs::write(bad.join("plugin.rhai"), "fn register() { syntax error").expect("break it");
        write_plugin(&parent, "good", MANIFEST, ENTRY, &[]);
        let loaded = load_all(&[parent]);
        assert_eq!(loaded.plugins_rejected.len(), 1);
        assert!(loaded.plugins_rejected[0].0.contains("bad"));
        assert_eq!(loaded.plugins_loaded.len(), 1);
        assert_eq!(loaded.commands.len(), 2);
    }

    #[test]
    fn a_duplicate_plugin_id_is_rejected() {
        let parent = plugin_parent("dup");
        write_plugin(&parent, "dup1", MANIFEST, ENTRY, &[]);
        write_plugin(&parent, "dup2", MANIFEST, ENTRY, &[]);
        let loaded = load_all(&[parent]);
        assert_eq!(loaded.plugins_loaded.len(), 1);
        assert_eq!(loaded.plugins_rejected.len(), 1);
        assert!(loaded.plugins_rejected[0].1.contains("duplicate plugin id"));
    }

    #[test]
    fn an_unknown_dock_area_is_a_note_not_a_rejection() {
        let parent = plugin_parent("area");
        write_plugin(
            &parent,
            "area",
            MANIFEST,
            r#"fn register() { register_panel("acme.x", "X", "chart.rhai", "everywhere"); }"#,
            &[],
        );
        let loaded = load_all(&[parent]);
        assert!(loaded.plugins_rejected.is_empty());
        assert!(loaded.panels.is_empty());
        assert!(loaded.notes.iter().any(|n| n.contains("unknown dock area")));
    }

    #[test]
    fn a_sync_callback_receives_its_arguments() {
        let parent = plugin_parent("cb");
        let loaded = {
            write_plugin(&parent, "cb", MANIFEST, ENTRY, &[]);
            load_all(&[parent])
        };
        let analyze = loaded
            .commands
            .iter()
            .find(|c| c.id == "acme.analyze")
            .expect("analyze registered");
        let args = CommandArgs {
            values: std::collections::BTreeMap::new(),
            value: Some("齿轮箱".to_string()),
        };
        let result = call_sync_callback(&analyze.source, &analyze.callback, &args);
        match result {
            CommandResult::Done { note } => assert_eq!(note.as_deref(), Some("分析 齿轮箱")),
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn a_background_callback_reports_progress_and_logs() {
        let parent = plugin_parent("bg");
        write_plugin(&parent, "bg", MANIFEST, ENTRY, &[]);
        let loaded = load_all(&[parent]);
        let job = loaded
            .commands
            .iter()
            .find(|c| c.id == "acme.job")
            .expect("job registered");

        #[derive(Default)]
        struct Recorder {
            fractions: std::sync::Mutex<Vec<Option<f32>>>,
            logs: std::sync::Mutex<Vec<String>>,
            cancelled: std::sync::atomic::AtomicBool,
        }
        impl ProgressSink for Recorder {
            fn report(&self, fraction: Option<f32>, _stage: Option<String>) {
                self.fractions.lock().unwrap().push(fraction);
            }
            fn log(&self, message: String) {
                self.logs.lock().unwrap().push(message);
            }
            fn is_cancelled(&self) -> bool {
                self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
            }
        }

        let recorder = std::sync::Arc::new(Recorder::default());
        let result = run_background_callback(&job.source, &job.callback, recorder.clone());
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            *recorder.fractions.lock().unwrap(),
            vec![Some(0.5)],
            "the script reported half progress"
        );
        assert_eq!(*recorder.logs.lock().unwrap(), vec!["working".to_string()]);
    }

    #[test]
    fn a_cancelled_background_callback_reports_cancellation() {
        let parent = plugin_parent("cancel");
        write_plugin(
            &parent,
            "cancel",
            MANIFEST,
            r#"fn register() { register_background_command("acme.c", "C", "c"); }
fn c(task, args) { task.check_cancelled() }"#,
            &[],
        );
        let loaded = load_all(&[parent]);
        let command = loaded.commands.first().expect("c registered");

        struct Cancelled;
        impl ProgressSink for Cancelled {
            fn report(&self, _fraction: Option<f32>, _stage: Option<String>) {}
            fn log(&self, _message: String) {}
            fn is_cancelled(&self) -> bool {
                true
            }
        }

        match run_background_callback(&command.source, &command.callback, std::sync::Arc::new(Cancelled))
        {
            Err(TaskFailure::Cancelled) => {}
            other => panic!("expected cancellation, got {other:?}"),
        }
    }
}
