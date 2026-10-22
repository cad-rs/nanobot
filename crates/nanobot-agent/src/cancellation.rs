//! CancellationToken: per-turn cancellation (design-harness 10.5).
//!
//! Each turn gets a fresh `CancellationToken`. When `cancel()` is called,
//! the token is set and the turn loop checks it at each step boundary.
//! The token is `Clone` + `Send + Sync` (Arc + AtomicBool internals).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A per-turn cancellation token.
#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Create a new, uncancelled token.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Cancel the token. All holders of this token will observe
    /// `is_cancelled() == true` after this call.
    pub fn cancel(&self) {
        self.inner.store(true, Ordering::SeqCst);
    }

    /// Check whether the token has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.inner.load(Ordering::SeqCst)
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}
