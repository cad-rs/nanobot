//! Context: named service repository with parent-child scoping (design-harness 10.5).
//!
//! `Context` is the root of the plugin system. It provides:
//! - Named service registration and lookup (`provide` / `get_named`)
//! - Child scoping via `extend()` (child registrations invisible to parent)
//! - An event bus (`events()`)
//!
//! `Context` is cheap to clone (Arc internally). It is `Send + Sync`.

use crate::disposable::Disposable;
use crate::events::Events;
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A named service repository with parent-child scoping.
#[derive(Clone)]
pub struct Context {
    inner: Arc<ContextInner>,
}

struct ContextInner {
    parent: Option<Context>,
    named_services: Mutex<HashMap<(String, TypeId), Box<dyn Any + Send + Sync>>>,
    events: Events,
}

impl Context {
    /// Create a root context with no parent.
    pub fn root() -> Self {
        Self {
            inner: Arc::new(ContextInner {
                parent: None,
                named_services: Mutex::new(HashMap::new()),
                events: Events::default(),
            }),
        }
    }

    /// Create a child context with its own service namespace.
    ///
    /// Services registered in the child are invisible to the parent, and
    /// lookups walk the parent chain. The child gets its **own** event bus: a
    /// listener registered on a child is not visible on the parent.
    pub fn extend(&self) -> Self {
        Self {
            inner: Arc::new(ContextInner {
                parent: Some(self.clone()),
                named_services: Mutex::new(HashMap::new()),
                events: Events::default(),
            }),
        }
    }

    /// Register a named service. Returns a `Disposable` that removes it.
    pub fn provide<T: Any + Send + Sync>(
        &self,
        name: impl Into<String>,
        value: T,
    ) -> Arc<dyn Disposable> {
        let name = name.into();
        let type_id = TypeId::of::<T>();
        let mut map = self.inner.named_services.lock().unwrap();
        map.insert((name.clone(), type_id), Box::new(value));
        drop(map);

        let inner = Arc::clone(&self.inner);
        Arc::new(crate::disposable::FnDisposable::new(move || {
            inner
                .named_services
                .lock()
                .unwrap()
                .remove(&(name.clone(), type_id));
        }))
    }

    /// Look up a service by name and type, walking the parent chain.
    pub fn get_named<T: Any + Send + Sync + Clone>(&self, name: &str) -> Option<T> {
        let type_id = TypeId::of::<T>();
        let key = (name.to_string(), type_id);

        let found = self
            .inner
            .named_services
            .lock()
            .unwrap()
            .get(&key)
            .and_then(|boxed| boxed.downcast_ref::<T>().cloned());

        match found {
            Some(value) => Some(value),
            None => self.inner.parent.as_ref().and_then(|p| p.get_named::<T>(name)),
        }
    }

    /// Get the event bus for this context.
    pub fn events(&self) -> &Events {
        &self.inner.events
    }
}
