//! The one polling loop a shell's live surfaces share.
//!
//! The state a surface renders is often changed by *other* threads: a job queue's
//! workers, a store another process is writing. Neither can push a change into a
//! window — a queue worker has no window handle, and the store is a directory. So a
//! surface that shows live state polls on a timer instead, and this is the loop
//! every such surface uses.

use std::time::Duration;

use gpui::Context;

/// Poll `body` every `period` until the entity is dropped.
///
/// `body` runs on the UI thread with the entity's own state and context, so it may
/// notify and re-render. The task ends when the window closes
/// ([`WeakEntity::update`] fails), which is what stops a detached poll from
/// outliving its surface.
pub fn poll_periodically<T: 'static>(
    cx: &mut Context<T>,
    period: Duration,
    body: impl Fn(&mut T, &mut Context<T>) + 'static,
) {
    cx.spawn(async move |entity, cx| {
        while entity.update(cx, |this, cx| body(this, cx)).is_ok() {
            cx.background_executor().timer(period).await;
        }
    })
    .detach();
}
