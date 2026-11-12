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
//!                   register_command / register_background_command /
//!                   add_toolbar_item / log_info
//! ```
//!
//! The manifest, the Rhai engines and the loading pipeline are the shared
//! `plugin-host` crate (product-kit); this crate is the framework-facing name
//! the shell already consumes. Loading is isolated: a rejected plugin lands in
//! `plugins_rejected` and does not interrupt the assembly of the others.

pub use plugin_host::*;
