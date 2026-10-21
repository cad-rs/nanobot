//! Disposable: the trait returned by every registration call (design-harness 10.5).
//!
//! When a listener, service, or child fiber is registered, the caller
//! receives a `Disposable`. Calling `dispose()` reverses that single
//! registration. `Fiber` tracks all disposables and unwinds them in
//! reverse order on teardown.

use std::sync::Arc;

/// A handle that reverses one registration when dropped or explicitly
/// disposed.
pub trait Disposable: Send + Sync {
    /// Reverse the registration this handle represents.
    fn dispose(&self);
}

/// A simple disposable that runs a closure on dispose.
pub struct FnDisposable {
    f: Box<dyn Fn() + Send + Sync>,
}

impl FnDisposable {
    /// Create a disposable that calls `f` on dispose.
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> Self {
        Self { f: Box::new(f) }
    }
}

impl Disposable for FnDisposable {
    fn dispose(&self) {
        (self.f)();
    }
}

/// A disposable that does nothing.
pub struct NoopDisposable;

impl Disposable for NoopDisposable {
    fn dispose(&self) {}
}

/// Wrap a disposable in an `Arc` so it can be shared.
pub fn arc<D: Disposable + 'static>(d: D) -> Arc<dyn Disposable> {
    Arc::new(d)
}
