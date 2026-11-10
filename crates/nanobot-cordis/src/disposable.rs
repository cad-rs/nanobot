//! Disposable: the trait returned by every registration call (design-harness 10.5).
//!
//! When a listener or service is registered, the caller receives a
//! `Disposable`. Calling `dispose()` reverses that single registration.
//! `Fiber` tracks all disposables and unwinds them in reverse order on
//! teardown.

/// A handle that reverses one registration when dropped or explicitly
/// disposed.
pub trait Disposable: Send + Sync {
    /// Reverse the registration this handle represents.
    fn dispose(&self);
}

/// A disposable that runs a closure on dispose.
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
