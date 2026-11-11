//! The models a user configured, and which one a session runs on.
//!
//! The list lives in the settings store ([`nanobot_boot::models`]); the
//! settings dialog edits it. Each session names one of them by id — the
//! product keeps that id with the session and reports it through
//! [`WorkspaceView::model_id`] — and the line under the composer says which.
//! Clicking it lists the models; choosing one tells the product
//! ([`HarnessShell::set_on_model`]), which points the session's agent at it.
//!
//! A session that names no model, or one that is gone, shows and runs on the
//! default. Switching a session's model never changes the default: that is
//! done in the settings dialog.
//!
//! A key typed here is handed to the credentials file and never read back
//! into the UI: the form only says whether one is saved.

use gpui::{
    deferred, div, prelude::*, px, AnyElement, Context, Div, Entity, MouseButton, MouseDownEvent,
    Pixels, Point, Window,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme as _, StyledExt as _};
use nanobot_boot::models::{self, ModelProfile, ModelProfiles};

use crate::icons::{icon, Icon};
use crate::model::{SessionId, SessionView, WorkspaceView};
use crate::shell::{palette, HarnessShell, Palette};

const MENU_W: f32 = 280.;
const ROW_H: f32 = 28.;

/// The open list of models to switch the session to.
pub(crate) struct ModelMenu {
    /// Where the pointer was; `None` (opened from the keyboard) centres it.
    at: Option<Point<Pixels>>,
}

/// The form a model is added or changed in.
pub(crate) struct ModelForm {
    /// The profile being changed; `None` adds one.
    editing: Option<String>,
    name: Entity<InputState>,
    endpoint: Entity<InputState>,
    model: Entity<InputState>,
    key: Entity<InputState>,
    key_env: Entity<InputState>,
}

impl<S: SessionView> HarnessShell<S> {
    /// What switching a session to another model does: the product records
    /// the profile id with the session and points its agent at it. Without
    /// this the line under the composer only names the model.
    pub fn set_on_model<F: Fn(&SessionId, &str) + 'static>(&mut self, callback: F) {
        self.on_model = Some(std::rc::Rc::new(callback));
    }

    /// The models the user configured.
    pub fn models(&self) -> &ModelProfiles {
        &self.models
    }

    /// The model the session on screen runs on: the one it names while that
    /// exists, the default otherwise.
    pub fn session_model(&self) -> Option<&ModelProfile> {
        let wanted = self.workspace().and_then(|ws| ws.model_id());
        self.models.resolve(wanted.as_deref())
    }

    fn persist_models(&self) {
        if let Some(Ok(mut settings)) = self.settings.as_ref().map(|s| s.lock()) {
            let _ = self.models.store(&mut settings);
        }
    }

    /// Adds `profile` or replaces the one with its id. `key`, when given, is
    /// saved as its API key — in the credentials file, which needs a shared
    /// settings store; an empty key keeps the one already saved.
    pub fn save_model(&mut self, profile: ModelProfile, key: Option<&str>) {
        if let (Some(key), Some(Ok(mut settings))) = (
            key.map(str::trim).filter(|key| !key.is_empty()),
            self.settings.as_ref().map(|s| s.lock()),
        ) {
            let _ = models::store_api_key(&mut settings, &profile.id, Some(key));
        }
        self.models.upsert(profile);
        self.persist_models();
    }

    /// Removes the model `id` and forgets its key. Sessions that named it
    /// run on the default from now on.
    pub fn remove_model(&mut self, id: &str) -> bool {
        if !self.models.remove(id) {
            return false;
        }
        if let Some(Ok(mut settings)) = self.settings.as_ref().map(|s| s.lock()) {
            let _ = models::store_api_key(&mut settings, id, None);
        }
        self.persist_models();
        true
    }

    /// Makes `id` the model new sessions start on.
    pub fn set_default_model(&mut self, id: &str) -> bool {
        if self.models.get(id).is_none() {
            return false;
        }
        self.models.default = Some(id.to_string());
        self.persist_models();
        true
    }

    /// Switches the session on screen to the model `id`. False when there is
    /// no such model, no session, or the product gave no way to switch.
    pub fn choose_model(&mut self, id: &str) -> bool {
        self.model_menu = None;
        let (Some(callback), Some(session)) =
            (self.on_model.clone(), self.active_session_id().cloned())
        else {
            return false;
        };
        if self.models.get(id).is_none() {
            return false;
        }
        callback(&session, id);
        true
    }

    /// Opens the list of models at `at` (window coordinates; `None` centres
    /// it). With no model configured yet it opens the settings instead.
    /// False when the product gave no way to switch.
    pub fn open_model_menu(&mut self, at: Option<(f32, f32)>) -> bool {
        if self.on_model.is_none() {
            return false;
        }
        if self.models.profiles.is_empty() {
            self.open_settings();
        } else {
            self.model_menu = Some(ModelMenu {
                at: at.map(|(x, y)| Point::new(px(x), px(y))),
            });
        }
        true
    }

    /// Whether the list of models is open.
    pub fn model_menu_is_open(&self) -> bool {
        self.model_menu.is_some()
    }

    /// Closes the list of models. False when it was not open.
    pub fn close_model_menu(&mut self) -> bool {
        self.model_menu.take().is_some()
    }

    /// What the line under the composer says about the model, and whether a
    /// click on it does something.
    pub(crate) fn model_chip(&self, ws: &S::Workspace, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = palette(cx);
        let can_switch = self.on_model.is_some();
        let label = match self.models.resolve(ws.model_id().as_deref()) {
            Some(profile) => profile.name.clone(),
            None => match ws.model_label() {
                Some(label) => label,
                None if can_switch => self.tr("model.add").to_string(),
                None => return None,
            },
        };
        let chip = div().id("harness-model").child(label);
        if !can_switch {
            return Some(chip.into_any_element());
        }
        Some(
            chip.cursor_pointer()
                .hover(|d| d.text_color(p.fg))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this: &mut Self, ev: &MouseDownEvent, _w, cx| {
                        this.open_model_menu(Some((f32::from(ev.position.x), f32::from(ev.position.y))));
                        cx.notify();
                    }),
                )
                .into_any_element(),
        )
    }

    /// The open list of models, over everything: a click elsewhere closes it.
    pub(crate) fn model_menu_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let menu = self.model_menu.as_ref()?;
        let p = palette(cx);
        let current = self.session_model().map(|profile| profile.id.clone());
        let rows = self.models.profiles.iter().enumerate().map(|(ix, profile)| {
            let id = profile.id.clone();
            let chosen = current.as_deref() == Some(profile.id.as_str());
            menu_row(("harness-model-row", ix), p)
                .child(
                    div()
                        .w(px(14.))
                        .flex_none()
                        .children(chosen.then(|| icon(Icon::CheckPass, p.accent))),
                )
                .child(div().flex_none().child(profile.name.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(p.muted)
                        .child(profile.model.clone()),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                        this.choose_model(&id);
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
        });
        let manage = menu_row("harness-model-manage", p)
            .border_t_1()
            .border_color(p.border)
            .text_color(p.muted)
            .child(icon(Icon::Settings, p.muted))
            .child(self.tr("model.manage"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                    this.close_model_menu();
                    this.open_settings();
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
        let card = div()
            .id("harness-model-menu")
            .w(px(MENU_W))
            .py_1()
            .rounded(px(6.))
            .border_1()
            .border_color(p.border)
            .bg(cx.theme().background)
            .shadow_lg()
            .flex()
            .flex_col()
            .children(rows)
            .child(manage);
        let close = |this: &mut Self, _ev: &MouseDownEvent, _w: &mut Window, cx: &mut Context<Self>| {
            this.close_model_menu();
            cx.notify();
        };
        let backdrop = div()
            .id("harness-model-menu-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(close))
            .on_mouse_down(MouseButton::Right, cx.listener(close));
        let backdrop = match menu.at {
            // The line it opens from is at the bottom: the list goes up.
            Some(at) => {
                let size = window.viewport_size();
                let height = (self.models.profiles.len() + 1) as f32 * ROW_H + 10.;
                let left = f32::from(at.x).min(f32::from(size.width) - MENU_W - 4.).max(4.);
                let top = (f32::from(at.y) - height - 8.).max(4.);
                backdrop.child(card.absolute().left(px(left)).top(px(top)))
            }
            None => backdrop
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(card),
        };
        Some(deferred(backdrop).with_priority(9).into_any_element())
    }

    // ---- the settings dialog's part ----

    /// Opens the form on the model `editing`, or empty to add one.
    pub(crate) fn open_model_form(
        &mut self,
        editing: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let profile = editing.and_then(|id| self.models.get(id)).cloned();
        let has_key = match (&profile, self.settings.as_ref().map(|s| s.lock())) {
            (Some(profile), Some(Ok(settings))) => models::has_stored_key(&settings, &profile.id),
            _ => false,
        };
        let value = |pick: fn(&ModelProfile) -> String| profile.as_ref().map(pick).unwrap_or_default();
        let mut input = |value: String, placeholder: &'static str, masked: bool| {
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
                    .masked(masked)
            })
        };
        let key_hint = if has_key { "model.key_saved" } else { "model.key_placeholder" };
        self.model_form = Some(ModelForm {
            name: input(value(|p| p.name.clone()), self.tr("model.name_placeholder"), false),
            endpoint: input(
                value(|p| p.endpoint.clone()),
                "https://api.example.com/v1",
                false,
            ),
            model: input(value(|p| p.model.clone()), self.tr("model.model_placeholder"), false),
            key: input(String::new(), self.tr(key_hint), true),
            key_env: input(
                value(|p| p.api_key_env.clone().unwrap_or_default()),
                "EXAMPLE_API_KEY",
                false,
            ),
            editing: profile.map(|profile| profile.id),
        });
    }

    /// Saves what the form holds. False, and the form stays, while a name, an
    /// endpoint or a model is missing.
    pub(crate) fn submit_model_form(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(form) = self.model_form.as_ref() else {
            return false;
        };
        let read = |input: &Entity<InputState>| input.read(cx).value().trim().to_string();
        let key_env = read(&form.key_env);
        let mut profile = ModelProfile::new(read(&form.name), read(&form.endpoint), read(&form.model));
        profile.api_key_env = (!key_env.is_empty()).then_some(key_env);
        if let Some(id) = &form.editing {
            profile.id = id.clone();
        }
        if !profile.is_complete() {
            return false;
        }
        let key = read(&form.key);
        self.model_form = None;
        self.save_model(profile, Some(&key));
        true
    }

    /// The models in the settings dialog: the list, or the form over it.
    pub(crate) fn models_section(&self, cx: &mut Context<Self>) -> Div {
        let p = palette(cx);
        let section = div().flex().flex_col().gap_2().pt_3().border_t_1().border_color(p.border);
        let Some(form) = &self.model_form else {
            return self.models_list(section, p, cx);
        };
        let title = if form.editing.is_some() { "model.edit" } else { "model.add" };
        let field = |label: &'static str, input: &Entity<InputState>| {
            crate::settings_dialog::setting_row(label, p)
                .child(div().flex_1().min_w_0().child(Input::new(input).w_full().text_xs()))
        };
        section
            .child(div().text_sm().font_semibold().child(self.tr(title)))
            .child(field(self.tr("model.name"), &form.name))
            .child(field(self.tr("model.endpoint"), &form.endpoint))
            .child(field(self.tr("model.model"), &form.model))
            .child(field(self.tr("model.key"), &form.key))
            .child(field(self.tr("model.key_env"), &form.key_env))
            .child(
                div()
                    .text_xs()
                    .text_color(p.muted)
                    .child(self.tr("model.key_note")),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(crate::widgets::button(
                        "harness-model-save",
                        self.tr("model.save"),
                        p.accent,
                        cx,
                        cx.listener(|this: &mut Self, _ev, _window, cx| {
                            this.submit_model_form(cx);
                            cx.notify();
                        }),
                    ))
                    .child(crate::widgets::button(
                        "harness-model-cancel",
                        self.tr("model.cancel"),
                        p.muted,
                        cx,
                        cx.listener(|this: &mut Self, _ev, _window, cx| {
                            this.model_form = None;
                            cx.notify();
                        }),
                    )),
            )
    }

    fn models_list(&self, section: Div, p: Palette, cx: &mut Context<Self>) -> Div {
        let default = self.models.default_profile().map(|profile| profile.id.clone());
        let rows = self.models.profiles.iter().enumerate().map(|(ix, profile)| {
            let is_default = default.as_deref() == Some(profile.id.as_str());
            let (for_default, for_edit, for_delete) =
                (profile.id.clone(), profile.id.clone(), profile.id.clone());
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().flex_none().text_sm().child(profile.name.clone()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(p.muted)
                        .child(format!("{} · {}", profile.model, profile.host())),
                )
                .child(crate::widgets::button(
                    ("harness-model-default", ix),
                    self.tr("model.default"),
                    if is_default { p.accent } else { p.muted },
                    cx,
                    cx.listener(move |this: &mut Self, _ev, _window, cx| {
                        this.set_default_model(&for_default);
                        cx.notify();
                    }),
                ))
                .child(
                    icon_button(("harness-model-edit", ix), Icon::ToolEdit, p).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                            this.open_model_form(Some(&for_edit), window, cx);
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    icon_button(("harness-model-delete", ix), Icon::ToolDelete, p).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _window, cx| {
                            this.remove_model(&for_delete);
                            cx.notify();
                        }),
                    ),
                )
        });
        section
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(div().flex_1().text_sm().text_color(p.muted).child(self.tr("model.title")))
                    .child(crate::widgets::button(
                        "harness-model-add",
                        self.tr("model.add"),
                        p.accent,
                        cx,
                        cx.listener(|this: &mut Self, _ev, window, cx| {
                            this.open_model_form(None, window, cx);
                            cx.notify();
                        }),
                    )),
            )
            .when(self.models.profiles.is_empty(), |d| {
                d.child(div().text_xs().text_color(p.muted).child(self.tr("model.none")))
            })
            .children(rows)
    }
}

fn menu_row(id: impl Into<gpui::ElementId>, p: Palette) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .h(px(ROW_H))
        .px_2()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .text_sm()
        .cursor_pointer()
        .hover(|d| d.bg(p.accent.opacity(0.25)))
}

fn icon_button(id: impl Into<gpui::ElementId>, which: Icon, p: Palette) -> gpui::Stateful<Div> {
    div()
        .id(id)
        .p_1()
        .rounded(px(4.))
        .cursor_pointer()
        .hover(|d| d.bg(p.muted.opacity(0.2)))
        .child(icon(which, p.muted))
}
