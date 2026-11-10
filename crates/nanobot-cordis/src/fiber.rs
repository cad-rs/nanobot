//! Fiber: reversible effect tracker with state machine (design-harness 10.5).
//!
//! A `Fiber` records all disposables (listeners, services) registered during a
//! plugin's body execution. On `dispose()`, all effects are unwound in
//! **reverse order** (LIFO), so dependencies are torn down before their
//! dependents.
//!
//! Fiber state machine: `Pending` → `Active` → `Disposed`.

use crate::disposable::Disposable;
use std::sync::{Arc, Mutex};

/// The lifecycle state of a fiber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FiberState {
    Pending,
    Active,
    Disposed,
    Unloading,
}

/// Tracks and reverses a set of effects (design-harness 10.5).
pub struct Fiber {
    name: String,
    state: Mutex<FiberState>,
    effects: Mutex<Vec<Arc<dyn Disposable>>>,
}

impl Fiber {
    /// Create a new fiber in the `Pending` state.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: Mutex::new(FiberState::Pending),
            effects: Mutex::new(Vec::new()),
        }
    }

    /// Get the fiber name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the current state.
    pub fn state(&self) -> FiberState {
        self.state.lock().unwrap().clone()
    }

    /// Transition to `Active`.
    pub fn set_active(&self) {
        *self.state.lock().unwrap() = FiberState::Active;
    }

    /// Record an effect (disposable) to be reversed on dispose.
    pub fn add_effect(&self, d: Arc<dyn Disposable>) {
        self.effects.lock().unwrap().push(d);
    }

    /// Dispose the fiber: unwind all effects in reverse order, then transition
    /// to `Disposed`.
    pub fn dispose(&self) {
        {
            let mut state = self.state.lock().unwrap();
            if *state == FiberState::Disposed || *state == FiberState::Unloading {
                return;
            }
            *state = FiberState::Unloading;
        }

        // Unwind effects in reverse order (LIFO).
        let effects: Vec<_> = {
            let mut effects = self.effects.lock().unwrap();
            effects.drain(..).collect()
        };
        for effect in effects.into_iter().rev() {
            effect.dispose();
        }

        *self.state.lock().unwrap() = FiberState::Disposed;
    }
}
