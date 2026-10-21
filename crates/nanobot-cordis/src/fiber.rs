//! Fiber: reversible effect tracker with state machine (design-harness 10.5).
//!
//! A `Fiber` records all disposables (listeners, services, child fibers)
//! registered during a plugin's body execution. On `dispose()`, all effects
//! are unwound in **reverse order** (LIFO), ensuring dependencies are torn
//! down before their dependents.
//!
//! Fiber state machine:
//! `Pending` → `Loading` → `Active` → `Disposed`
//!                  ↘ `Failed` → `Disposed`

use crate::disposable::Disposable;
use std::sync::{Arc, Mutex};

/// The lifecycle state of a fiber.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FiberState {
    Pending,
    Loading,
    Active,
    Failed(String),
    Disposed,
    Unloading,
}

/// Tracks and reverses a set of effects (design-harness 10.5).
pub struct Fiber {
    name: String,
    state: Mutex<FiberState>,
    effects: Mutex<Vec<Arc<dyn Disposable>>>,
    children: Mutex<Vec<Arc<Fiber>>>,
}

impl Fiber {
    /// Create a new fiber in the `Pending` state.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: Mutex::new(FiberState::Pending),
            effects: Mutex::new(Vec::new()),
            children: Mutex::new(Vec::new()),
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

    /// Transition to `Failed` with a reason.
    pub fn set_failed(&self, reason: String) {
        *self.state.lock().unwrap() = FiberState::Failed(reason);
    }

    /// Record an effect (disposable) to be reversed on dispose.
    pub fn add_effect(&self, d: Arc<dyn Disposable>) {
        self.effects.lock().unwrap().push(d);
    }

    /// Register a child fiber. The child is disposed before this fiber.
    pub fn add_child(&self, child: Arc<Fiber>) {
        self.children.lock().unwrap().push(child);
    }

    /// Dispose the fiber: unwind all effects in reverse order, then
    /// dispose all child fibers, then transition to `Disposed`.
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

        // Dispose children in reverse order.
        let children: Vec<_> = {
            let mut children = self.children.lock().unwrap();
            children.drain(..).collect()
        };
        for child in children.into_iter().rev() {
            child.dispose();
        }

        *self.state.lock().unwrap() = FiberState::Disposed;
    }
}
