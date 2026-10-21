//! nanobot-cordis: the plugin kernel.
//!
//! This crate owns the core plugin infrastructure:
//! - `Context` — named service repository with parent-child scoping
//! - `Events` — typed event bus with 4 dispatch modes (emit/serial/waterfall/waterfall_async)
//! - `Fiber` — reversible effect tracker with state machine
//! - `Disposable` — the handle returned by every registration call
//!
//! Provenance: this kernel was extracted from the ocad design harness
//! (see `docs/nanobot-architecture.md` in rcad-pro for the framework/product
//! split and the migration path). The kernel crates are *self-held*: loop,
//! event log, plan, approval, anchoring and the tool waterfall are product
//! semantics — replacing them means replacing the product. Commodity parts
//! (provider adapters, MCP client, etc.) are borrowed and replaceable.
//!
//! No tokio runtime entry; the async runtime is provided by the host
//! (desktop gpui / web wasm_bindgen_futures::spawn_local).

pub mod context;
pub mod disposable;
pub mod events;
pub mod fiber;

pub use context::Context;
pub use disposable::{arc, Disposable, FnDisposable, NoopDisposable};
pub use events::Events;
pub use fiber::{Fiber, FiberState};
