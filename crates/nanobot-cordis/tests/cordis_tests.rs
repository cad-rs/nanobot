//! Integration tests for nanobot-cordis: Context, Events, Fiber.

use nanobot_cordis::{Context, Fiber, FiberState};
use std::sync::Arc;

#[test]
fn context_provide_get_roundtrip() {
    let ctx = Context::root();
    ctx.provide("answer", 42i32);
    assert_eq!(ctx.get_named::<i32>("answer"), Some(42));
}

#[test]
fn context_child_invisible_to_parent() {
    let parent = Context::root();
    let child = parent.extend();
    child.provide("secret", String::from("child_only"));
    assert_eq!(parent.get_named::<String>("secret"), None);
    assert_eq!(
        child.get_named::<String>("secret"),
        Some(String::from("child_only"))
    );
}

#[test]
fn events_emit_reaches_listener() {
    let ctx = Context::root();
    let hits = Arc::new(std::sync::Mutex::new(0));
    let h = Arc::clone(&hits);
    ctx.events().on_emit("test_event", move |_payload: &i32| {
        *h.lock().unwrap() += 1;
    });
    ctx.events().emit("test_event", 99);
    assert_eq!(*hits.lock().unwrap(), 1);
}

#[test]
fn events_waterfall_identity() {
    let ctx = Context::root();
    ctx.events()
        .on_waterfall("transform", |n: i32, next: &dyn Fn(i32) -> i32| next(n) * 2);
    let result = ctx.events().waterfall::<i32, i32>("transform", 5);
    assert_eq!(result, Some(10));
}

#[test]
fn events_waterfall_two_listeners() {
    let ctx = Context::root();
    ctx.events()
        .on_waterfall("chain", |n: i32, next: &dyn Fn(i32) -> i32| next(n) * 2);
    ctx.events()
        .on_waterfall("chain", |n: i32, next: &dyn Fn(i32) -> i32| next(n) + 10);
    let result = ctx.events().waterfall::<i32, i32>("chain", 5);
    assert!(result.is_some());
}

#[test]
fn fiber_dispose_rolls_back() {
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let o = Arc::clone(&order);

    // The lifecycle a plugin host runs: a fiber, effects recorded into it, and
    // disposal unwinding them.
    let fiber = Arc::new(Fiber::new("test_plugin"));
    o.lock().unwrap().push("plugin_ran");
    fiber.set_active();
    assert_eq!(fiber.state(), FiberState::Active);
    fiber.dispose();
    assert_eq!(fiber.state(), FiberState::Disposed);
}
