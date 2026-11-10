//! The settings dialog: a card in the middle of the window, over everything.
//!
//! Opened from the gear in the sidebar's footer, `ctrl-,` or the palette.
//! Escape, the close button or a click outside the card closes it. What it
//! changes takes effect at once and is written to the shared settings store
//! when there is one — there is nothing to confirm, so there is no "save".

use gpui::{
    deferred, div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, Window,
};
use gpui_component::{ActiveTheme as _, StyledExt as _};

use crate::icons::{icon, Icon};
use crate::model::SessionView;
use crate::shell::{palette, settings_body, HarnessShell};

impl<S: SessionView> HarnessShell<S> {
    /// Opens the settings dialog. A session menu that was open is closed:
    /// one thing at a time is over the window.
    pub fn open_settings(&mut self) {
        self.session_menu = None;
        self.settings_open = true;
    }

    /// Closes the settings dialog. False when it was not open.
    pub fn close_settings(&mut self) -> bool {
        std::mem::take(&mut self.settings_open)
    }

    /// Whether the settings dialog is open.
    pub fn settings_is_open(&self) -> bool {
        self.settings_open
    }

    /// The dialog, when open: deferred, so it also covers the viewport.
    pub(crate) fn settings_overlay(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.settings_open {
            return None;
        }
        let p = palette(cx);
        let close = |this: &mut Self, _ev: &MouseDownEvent, _w: &mut Window, cx: &mut Context<Self>| {
            this.close_settings();
            cx.notify();
        };
        let card = div()
            .id("harness-settings-dialog")
            .w(px(520.))
            .max_w_full()
            .rounded(px(8.))
            .border_1()
            .border_color(p.border)
            .bg(cx.theme().background)
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            // A click on the card is not a click on the backdrop.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(p.border)
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(div().flex_1().text_sm().font_semibold().child(self.tr("shell.settings")))
                    .child(
                        div()
                            .id("harness-settings-close")
                            .p_1()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|d| d.bg(p.muted.opacity(0.2)))
                            .child(icon(Icon::Close, p.muted))
                            .on_mouse_down(MouseButton::Left, cx.listener(close)),
                    ),
            )
            .child(div().p_4().child(settings_body(self, window, cx)));
        let backdrop = div()
            .id("harness-settings-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui::black().opacity(0.35))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p_4()
            .on_mouse_down(MouseButton::Left, cx.listener(close))
            .child(card);
        Some(deferred(backdrop).with_priority(10).into_any_element())
    }
}
