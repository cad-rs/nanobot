//! Context: named service repository with parent-child scoping (design-harness 10.5).
//!
//! `Context` is the root of the plugin system. It provides:
//! - Named service registration and lookup (`provide` / `get` / `get_named`)
//! - Child scoping via `extend()` (child registrations invisible to parent)
//! - Plugin lifecycle via `plugin()` (body runs inside a `Fiber`)
//! - Shared event bus (`events()`)
//!
//! `Context` is cheap to clone (Arc internally). It is `Send + Sync`.

use crate::disposable::Disposable;
use crate::events::Events;
use crate::fiber::Fiber;
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
    services: Mutex<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
    named_services: Mutex<HashMap<(String, TypeId), Box<dyn Any + Send + Sync>>>,
    events: Events,
}

impl Context {
    /// Create a root context with no parent.
    pub fn root() -> Self {
        Self {
            inner: Arc::new(ContextInner {
                parent: None,
                services: Mutex::new(HashMap::new()),
                named_services: Mutex::new(HashMap::new()),
                events: Events::default(),
            }),
        }
    }

    /// Create a child context that shares the event bus but has its own
    /// service namespace. Services registered in the child are invisible
    /// to the parent; lookups walk the parent chain.
    pub fn extend(&self) -> Self {
        Self {
            inner: Arc::new(ContextInner {
                parent: Some(self.clone()),
                services: Mutex::new(HashMap::new()),
                named_services: Mutex::new(HashMap::new()),
                events: Events::default(),
            }),
        }
    }

    /// Register a plugin: run `body` inside a new `Fiber`, recording all
    /// disposables. If `body` returns `Err`, the fiber is rolled back.
    pub fn plugin<F, C, E>(&self, name: impl Into<String>, config: C, body: F) -> Arc<Fiber>
    where
        F: FnOnce(&Context, C) -> Result<(), E>,
        C: 'static,
        E: 'static,
    {
        let fiber = Arc::new(Fiber::new(name.into()));
        let ctx = self.extend();
        let result = body(&ctx, config);

        match result {
            Ok(()) => {
                fiber.set_active();
                fiber
            }
            Err(_) => {
                fiber.set_failed("plugin body returned Err".to_string());
                fiber.dispose();
                fiber
            }
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

        let inner = Arc::clone(&self.inner);
        Arc::new(crate::disposable::FnDisposable::new(move || {
            let mut map = inner.named_services.lock().unwrap();
            map.remove(&(name.clone(), type_id));
        }))
    }

    /// Look up a service by type, walking the parent chain.
    pub fn get<T: Any + Send + Sync + Clone>(&self) -> Option<T> {
        let type_id = TypeId::of::<T>();
        let map = self.inner.services.lock().unwrap();
        if let Some(boxed) = map.get(&type_id) {
            return boxed.downcast_ref::<T>().cloned();
        }
        drop(map);
        self.inner.parent.as_ref().and_then(|p| p.get::<T>())
    }

    /// Look up a service by name and type, walking the parent chain.
    pub fn get_named<T: Any + Send + Sync + Clone>(&self, name: &str) -> Option<T> {
        let type_id = TypeId::of::<T>();
        let map = self.inner.named_services.lock().unwrap();
        if let Some(boxed) = map.get(&(name.to_string(), type_id)) {
            return boxed.downcast_ref::<T>().cloned();
        }
        drop(map);
        self.inner
            .parent
            .as_ref()
            .and_then(|p| p.get_named::<T>(name))
    }

    /// Get the event bus for this context.
    pub fn events(&self) -> &Events {
        &self.inner.events
    }
}
