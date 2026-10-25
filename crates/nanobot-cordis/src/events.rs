//! Events: typed event bus with 4 dispatch modes (design-harness 10.5).
//!
//! Listeners are stored keyed by `(event name, Payload TypeId, Result TypeId)`.
//! Four dispatch modes are supported:
//!
//! - `emit` — fire-and-forget observer (telemetry, UI projection)
//! - `serial` — ask each listener in turn, first `Some<R>` wins (capability query)
//! - `waterfall` — payload flows through the listener chain, each gets `(payload, next)`
//! - `waterfall_async` — async variant for tool bodies that need to await
//!
//! All listeners are type-erased via `Box<dyn Any>` and safely downcast at
//! invocation time. If a payload type does not match the registered `TypeId`,
//! it is silently skipped (emit/serial) or passed through (waterfall).

use crate::disposable::{Disposable, FnDisposable};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

// Type aliases for type-erased listeners.

type EmitListener = Box<dyn Fn(&dyn Any) + Send + Sync>;

type SerialListener = Box<dyn Fn(&dyn Any) -> Option<Box<dyn Any>> + Send + Sync>;

type WaterfallListener = Arc<
    dyn Fn(
            Box<dyn Any + Send + Sync>,
            &dyn Fn(Box<dyn Any + Send + Sync>) -> Box<dyn Any + Send + Sync>,
        ) -> Box<dyn Any + Send + Sync>
        + Send
        + Sync,
>;

/// The sync passthrough a waterfall chains through.
type Passthrough = Arc<
    dyn Fn(Box<dyn Any + Send + Sync>) -> Box<dyn Any + Send + Sync> + Send + Sync,
>;

/// The typed `next` the async waterfall hands its listeners.
type AsyncNext = Box<
    dyn Fn(Box<dyn Any + Send + Sync>) -> Pin<Box<dyn std::future::Future<Output = Box<dyn Any + Send + Sync>> + Send>>
        + Send
        + Sync,
>;

/// The future an async waterfall listener resolves with.
type BoxFuture = Pin<Box<dyn std::future::Future<Output = Box<dyn Any + Send + Sync>> + Send>>;

type AsyncWaterfallListener = Arc<
    dyn Fn(
            Box<dyn Any + Send + Sync>,
            Box<
                dyn Fn(
                        Box<dyn Any + Send + Sync>,
                    ) -> Pin<
                        Box<dyn std::future::Future<Output = Box<dyn Any + Send + Sync>> + Send>,
                    > + Send
                    + Sync,
            >,
        ) -> Pin<Box<dyn std::future::Future<Output = Box<dyn Any + Send + Sync>> + Send>>
        + Send
        + Sync,
>;

/// Typed event bus with 4 dispatch modes.
///
/// `Events` is `Clone` — each clone shares the same underlying listener
/// stores via `Arc`. This allows `Context` to own one `Events` and hand
/// out clones to children, while disposables capture `Arc<Mutex<...>>`
/// handles that remain valid for `'static`.
type EmitListeners = Arc<Mutex<HashMap<(&'static str, TypeId), Vec<EmitListener>>>>;
type SerialListeners = Arc<Mutex<HashMap<(&'static str, TypeId, TypeId), Vec<SerialListener>>>>;
type WaterfallListeners =
    Arc<Mutex<HashMap<(&'static str, TypeId, TypeId), Vec<WaterfallListener>>>>;
type AsyncWaterfallListeners =
    Arc<Mutex<HashMap<(&'static str, TypeId, TypeId), Vec<AsyncWaterfallListener>>>>;

#[derive(Clone, Default)]
pub struct Events {
    emit_listeners: EmitListeners,
    serial_listeners: SerialListeners,
    waterfall_listeners: WaterfallListeners,
    async_waterfall_listeners: AsyncWaterfallListeners,
}

impl Events {
    /// Register a fire-and-forget observer for events named `name` with payload type `P`.
    pub fn on_emit<P: Any + Send + Sync>(
        &self,
        name: &'static str,
        listener: impl Fn(&P) + Send + Sync + 'static,
    ) -> Arc<dyn Disposable> {
        let type_id = TypeId::of::<P>();
        let wrapped: EmitListener = Box::new(move |payload: &dyn Any| {
            if let Some(p) = payload.downcast_ref::<P>() {
                listener(p);
            }
        });

        let mut map = self.emit_listeners.lock().unwrap();
        map.entry((name, type_id)).or_default().push(wrapped);
        drop(map);

        let store = Arc::clone(&self.emit_listeners);
        Arc::new(FnDisposable::new(move || {
            let mut map = store.lock().unwrap();
            if let Some(listeners) = map.get_mut(&(name, type_id)) {
                listeners.pop();
                if listeners.is_empty() {
                    map.remove(&(name, type_id));
                }
            }
        }))
    }

    /// Emit an event named `name` with payload `payload`.
    pub fn emit<P: Any + Send + Sync>(&self, name: &'static str, payload: P) {
        let type_id = TypeId::of::<P>();
        let map = self.emit_listeners.lock().unwrap();
        if let Some(listeners) = map.get(&(name, type_id)) {
            let payload_ref: &dyn Any = &payload;
            for listener in listeners {
                listener(payload_ref);
            }
        }
    }

    /// Register a serial listener for events named `name` with payload `P` and result `R`.
    pub fn on_serial<P, R>(
        &self,
        name: &'static str,
        listener: impl Fn(&P) -> Option<R> + Send + Sync + 'static,
    ) -> Arc<dyn Disposable>
    where
        P: Any + Send + Sync,
        R: Any + Send + Sync,
    {
        let type_id = TypeId::of::<P>();
        let result_type_id = TypeId::of::<R>();
        let wrapped: SerialListener = Box::new(move |payload: &dyn Any| -> Option<Box<dyn Any>> {
            if let Some(p) = payload.downcast_ref::<P>() {
                listener(p).map(|r| Box::new(r) as Box<dyn Any>)
            } else {
                None
            }
        });

        let mut map = self.serial_listeners.lock().unwrap();
        map.entry((name, type_id, result_type_id))
            .or_default()
            .push(wrapped);
        drop(map);

        let store = Arc::clone(&self.serial_listeners);
        Arc::new(FnDisposable::new(move || {
            let mut map = store.lock().unwrap();
            if let Some(listeners) = map.get_mut(&(name, type_id, result_type_id)) {
                listeners.pop();
                if listeners.is_empty() {
                    map.remove(&(name, type_id, result_type_id));
                }
            }
        }))
    }

    /// Query a serial event named `name` with payload `P`, returning the first `Some<R>`.
    pub fn serial<P, R>(&self, name: &'static str, payload: P) -> Option<R>
    where
        P: Any + Send + Sync,
        R: Any + Send + Sync,
    {
        let type_id = TypeId::of::<P>();
        let result_type_id = TypeId::of::<R>();
        let map = self.serial_listeners.lock().unwrap();
        if let Some(listeners) = map.get(&(name, type_id, result_type_id)) {
            let payload_ref: &dyn Any = &payload;
            for listener in listeners {
                if let Some(result_box) = listener(payload_ref) {
                    return result_box.downcast::<R>().ok().map(|b| *b);
                }
            }
        }
        None
    }

    /// Register a waterfall listener for events named `name` with payload `P` and result `R`.
    pub fn on_waterfall<P, R>(
        &self,
        name: &'static str,
        listener: impl Fn(P, &dyn Fn(P) -> R) -> R + Send + Sync + 'static,
    ) -> Arc<dyn Disposable>
    where
        P: Any + Send + Sync,
        R: Any + Send + Sync,
    {
        let type_id = TypeId::of::<P>();
        let result_type_id = TypeId::of::<R>();
        let wrapped: WaterfallListener = Arc::new(
            move |payload: Box<dyn Any + Send + Sync>,
                  next: &dyn Fn(Box<dyn Any + Send + Sync>) -> Box<dyn Any + Send + Sync>|
                  -> Box<dyn Any + Send + Sync> {
                match payload.downcast::<P>() {
                    Ok(p_box) => {
                        let p = *p_box;
                        let next_wrapped = |p: P| -> R {
                            let p_box: Box<dyn Any + Send + Sync> = Box::new(p);
                            let result_box = next(p_box);
                            match result_box.downcast::<R>() {
                                Ok(r_box) => *r_box,
                                Err(_) => panic!("waterfall next callback returned wrong type"),
                            }
                        };
                        let result = listener(p, &next_wrapped);
                        Box::new(result)
                    }
                    Err(payload) => next(payload),
                }
            },
        );

        let mut map = self.waterfall_listeners.lock().unwrap();
        map.entry((name, type_id, result_type_id))
            .or_default()
            .push(wrapped);
        drop(map);

        let store = Arc::clone(&self.waterfall_listeners);
        Arc::new(FnDisposable::new(move || {
            let mut map = store.lock().unwrap();
            if let Some(listeners) = map.get_mut(&(name, type_id, result_type_id)) {
                listeners.pop();
                if listeners.is_empty() {
                    map.remove(&(name, type_id, result_type_id));
                }
            }
        }))
    }

    /// Drive a waterfall event named `name` with payload `P` through the listener chain.
    pub fn waterfall<P, R>(&self, name: &'static str, payload: P) -> Option<R>
    where
        P: Any + Send + Sync,
        R: Any + Send + Sync,
    {
        let type_id = TypeId::of::<P>();
        let result_type_id = TypeId::of::<R>();

        let listeners = self
            .waterfall_listeners
            .lock()
            .unwrap()
            .get(&(name, type_id, result_type_id))
            .cloned();

        if let Some(listeners) = listeners {
            if listeners.is_empty() {
                return None;
            }

            let mut next: Passthrough = Arc::new(|b| b);
            for listener in listeners.iter() {
                let listener = Arc::clone(listener);
                let prev_next = Arc::clone(&next);
                next = Arc::new(move |b| listener(b, &*prev_next));
            }

            let result_box = next(Box::new(payload));
            result_box.downcast::<R>().ok().map(|b| *b)
        } else {
            None
        }
    }

    /// Register an async waterfall listener for events named `name`.
    pub fn on_waterfall_async<P, R>(
        &self,
        name: &'static str,
        listener: impl Fn(
                P,
                Box<
                    dyn Fn(
                            Box<dyn Any + Send + Sync>,
                        ) -> Pin<
                            Box<
                                dyn std::future::Future<Output = Box<dyn Any + Send + Sync>> + Send,
                            >,
                        > + Send
                        + Sync,
                >,
            )
                -> Pin<Box<dyn std::future::Future<Output = Box<dyn Any + Send + Sync>> + Send>>
            + Send
            + Sync
            + 'static,
    ) -> Arc<dyn Disposable>
    where
        P: Any + Send + Sync + Clone,
        R: Any + Send + Sync,
    {
        let type_id = TypeId::of::<P>();
        let result_type_id = TypeId::of::<R>();
        let listener = Arc::new(listener);
        let wrapped: AsyncWaterfallListener = Arc::new(
            move |payload: Box<dyn Any + Send + Sync>, next: AsyncNext| -> BoxFuture {
                let listener = Arc::clone(&listener);
                Box::pin(async move {
                    match payload.downcast::<P>() {
                        Ok(p_box) => {
                            let p = *p_box;
                            listener(p, next).await
                        }
                        Err(payload) => next(payload).await,
                    }
                })
            },
        );

        let mut map = self.async_waterfall_listeners.lock().unwrap();
        map.entry((name, type_id, result_type_id))
            .or_default()
            .push(wrapped);
        drop(map);

        let store = Arc::clone(&self.async_waterfall_listeners);
        Arc::new(FnDisposable::new(move || {
            let mut map = store.lock().unwrap();
            if let Some(listeners) = map.get_mut(&(name, type_id, result_type_id)) {
                listeners.pop();
                if listeners.is_empty() {
                    map.remove(&(name, type_id, result_type_id));
                }
            }
        }))
    }
}
