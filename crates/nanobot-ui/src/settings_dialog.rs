//! The settings dialog: a card in the middle of the window, over everything.
//!
//! Opened from the gear in the sidebar's footer, `ctrl-,` or the palette.
//! Escape, the close button or a click outside the card closes it. What it
//! changes takes effect at once and is written to the shared settings store
//! when there is one — there is nothing to confirm, so there is no "save".

use gpui::{
    deferred, div, prelude::*, px, AnyElement, Context, Div, ElementId, Entity, MouseButton,
    MouseDownEvent, Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{ActiveTheme as _, StyledExt as _};

use crate::i18n::Locale;
use crate::icons::{icon, Icon};
use crate::model::SessionView;
use crate::shell::{palette, HarnessShell, Palette};
use crate::theme::{ThemeKind, FONT_FAMILY_KEY, FONT_SIZE_KEY, LOCALE_KEY};

/// One row of the dialog: what the setting is, then its choices.
fn setting_row(label: &'static str, p: Palette) -> Div {
    div().flex().flex_row().items_center().gap_2().child(
        div()
            .w(px(96.))
            .flex_none()
            .text_sm()
            .text_color(p.muted)
            .child(label),
    )
}

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
            .child(self.settings_body(window, cx).p_4());
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

    /// One choice of a setting, lit when it is the current one.
    fn choice(
        &self,
        id: impl Into<ElementId>,
        label: &'static str,
        current: bool,
        cx: &mut Context<Self>,
        choose: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let p = palette(cx);
        crate::widgets::button(
            id,
            label,
            if current { p.accent } else { p.muted },
            cx,
            cx.listener(move |shell, _event, window, cx| {
                choose(shell, window, cx);
                cx.notify();
            }),
        )
    }

    /// The font family box: made the first time the dialog shows; Enter
    /// applies what is typed.
    fn font_family_input(&self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(input) = self.font_input.borrow().clone() {
            return input;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(self.tr("settings.font.placeholder"))
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |shell: &mut Self, input, event: &InputEvent, window, cx| {
                let family = input.read(cx).value().to_string();
                if matches!(event, InputEvent::PressEnter { .. }) && !family.trim().is_empty() {
                    shell.set_font_setting(FONT_FAMILY_KEY, serde_json::json!(family), window, cx);
                }
            },
        );
        *self.font_input.borrow_mut() = Some(input.clone());
        *self.font_input_sub.borrow_mut() = Some(subscription);
        input
    }

    /// What the dialog holds: theme, motion, language, font, and where the
    /// settings file is.
    fn settings_body(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let font_size = self
            .settings()
            .and_then(|store| store.lock().ok()?.get(FONT_SIZE_KEY)?.as_f64())
            .unwrap_or(16.);
        let themes = [
            (ThemeKind::Dark, "settings.theme_dark"),
            (ThemeKind::Light, "settings.theme_light"),
        ];
        let motions = [(false, "settings.motion_full"), (true, "settings.motion_reduced")];
        let locales = [(Locale::ZhCn, "settings.locale.zh"), (Locale::En, "settings.locale.en")];
        let sizes = [
            (14., "settings.font.small"),
            (16., "settings.font.normal"),
            (18., "settings.font.large"),
        ];
        let path = self
            .settings_path()
            .unwrap_or_else(|| self.tr("settings.memory_mode").to_string());
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(setting_row(self.tr("settings.theme"), p).children(themes.map(|(kind, label)| {
                self.choice(
                    format!("harness-theme-{}", kind.as_str()),
                    self.tr(label),
                    self.theme() == kind,
                    cx,
                    move |shell, window, cx| shell.switch_theme(kind, window, cx),
                )
            })))
            .child(setting_row(self.tr("settings.motion"), p).children(motions.map(
                |(reduce, label)| {
                    self.choice(
                        if reduce { "harness-motion-reduced" } else { "harness-motion-full" },
                        self.tr(label),
                        self.reduce_motion() == reduce,
                        cx,
                        move |shell, _window, _cx| shell.set_reduce_motion(reduce),
                    )
                },
            )))
            .child(setting_row(self.tr("settings.locale"), p).children(locales.map(
                |(locale, label)| {
                    self.choice(
                        format!("harness-locale-{}", locale.code()),
                        // Each language is named in itself.
                        crate::i18n::t(locale, label),
                        self.locale() == locale,
                        cx,
                        move |shell, _window, _cx| {
                            shell.set_locale(locale);
                            shell.save_setting(LOCALE_KEY, serde_json::json!(locale.code()));
                        },
                    )
                },
            )))
            .child(setting_row(self.tr("settings.font"), p).children(sizes.map(|(size, label)| {
                self.choice(
                    format!("harness-font-size-{size}"),
                    self.tr(label),
                    (font_size - size).abs() < f64::EPSILON,
                    cx,
                    move |shell, window, cx| {
                        shell.set_font_setting(FONT_SIZE_KEY, serde_json::json!(size), window, cx);
                    },
                )
            })))
            .child(setting_row(self.tr("settings.font.family"), p).child(
                div().w(px(180.)).child(
                    Input::new(&self.font_family_input(window, cx))
                        .w_full()
                        .text_xs(),
                ),
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(p.muted)
                    .child(crate::widgets::truncate(&path, 64)),
            )
    }
}
