//! The gpui workbench shell (design 4.8).
//!
//! The shell is the **generic** half of a harness workbench: the session list,
//! the session workspace (conversation stream + plan review + the approval /
//! clarify bars + composer), the viewport region, and the panel dock that hosts
//! both the product's domain panels and the script-plugin panels. It is generic
//! over [`SessionView`], so a product keeps its own session type, its own
//! workspace projection and its own renderer — and the shell never learns a
//! domain word.
//!
//! ```text
//! header (identity · undo/redo · file ops · status)
//! ┌──────────┬───────────────────────┬──────────┬──────────┐
//! │ session  │ session workspace     │ viewport │ right    │
//! │ list     │ conversation + plan   │ (product │ dock     │
//! │          │ + input               │  slot)   │ panels   │
//! └──────────┴───────────────────────┴──────────┴──────────┘
//! ```

use std::cell::{Ref, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui::{
    div, img, prelude::*, px, AnyElement, App, ColorExt as _, Context, Div, Entity, Hsla, Stateful,
    IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, ScrollDelta,
    ScrollWheelEvent, Subscription, Window,
};
use gpui_component::{
    input::{Input, InputEvent, InputState},
    ActiveTheme as _, StyledExt as _,
};
use nanobot_boot::account::Account;
use nanobot_boot::Settings;
use nanobot_gpu::{ViewportInput, ViewportProjection, ViewportSlot};

use crate::model::{
    plan_progress, Block, BlockKind, PlanDecision, PlanPhase, PlanStep, ProjectId,
    SessionId, SessionStatus, SessionStore, SessionView, StepStatus, ToolState, WorkspaceView,
};
use crate::panels::{PanelArea, PanelDef, PanelHost};
use crate::i18n::Locale;
use crate::theme::ThemeKind;

/// Pane widths: the workspace is the primary surface (flex), the viewport is the
/// secondary panel, and the dock hosts the auxiliary panels.
const SIDEBAR_W: f32 = 240.0;
const VIEWPORT_W: f32 = 520.0;

/// A shell callback keyed by session (activate / undo / redo / save / load /
/// import all share the shape).
type SessionCallback = Rc<RefCell<dyn Fn(&SessionId)>>;

/// The composer's submit callback.
type MessageCallback = Rc<RefCell<dyn Fn(&SessionId, &str)>>;

/// A contingent-plan decision callback.
type PlanDecisionCallback = Rc<RefCell<dyn Fn(&SessionId, PlanDecision)>>;

/// The harness shell: the session list plus the active session's workspace.
pub struct HarnessShell<S: SessionView> {
    store: RefCell<SessionStore<S>>,
    /// The active session's whole workspace. `None` for a project without
    /// sessions: an open project with no sessions is a legal state, not an error.
    workspace: Option<S::Workspace>,
    /// One viewport handle per session, created on first use. Viewport state
    /// belongs to the session (switching back restores it), so a switch must not
    /// carry the old state over.
    viewports: HashMap<SessionId, <S::Viewport as ViewportSlot>::Handle>,
    /// The product's display name in the header.
    product_name: String,
    /// Callback invoked when a session is activated (switched to). The app layer
    /// uses this to bind an agent to the session.
    on_session_activated: Option<SessionCallback>,
    /// The message composer. `None` in headless mode, where no window exists to
    /// host the input entity.
    composer: Option<Entity<InputState>>,
    /// Callback invoked when the user submits the composer.
    on_message_sent: Option<MessageCallback>,
    /// Callback invoked when the user decides a contingent plan.
    on_plan_decision: Option<PlanDecisionCallback>,
    /// Undo/redo and container save/load/import: the app layer forwards them to
    /// its engine dispatch table.
    on_undo: Option<SessionCallback>,
    on_redo: Option<SessionCallback>,
    /// An optional product projection of the sidebar (design-rooted groups
    /// instead of the flat session list). `None` = the default view.
    sidebar_projection: Option<Rc<dyn crate::model::SidebarProjection<S>>>,
    on_save: Option<SessionCallback>,
    on_load: Option<SessionCallback>,
    on_import: Option<SessionCallback>,
    /// Keeps the composer's submit subscription alive.
    _composer_sub: Option<Subscription>,
    /// The currently selected entity (three-way linking with the conversation).
    selected_entity: Option<String>,
    /// The currently selected conversation block event sequence.
    selected_block: Option<u64>,
    /// Product panels and script-plugin panels.
    panel_host: PanelHost<S>,
    /// The user settings store, when the app layer shared it. Presence is
    /// what registers the framework's settings panel and makes theme choices
    /// persist; without it the theme still switches, in memory only.
    settings: Option<Arc<Mutex<Settings>>>,
    /// The theme the shell renders in. Tracked so a toggle knows its
    /// starting point even without a settings store.
    theme: ThemeKind,
    /// The identity the account provider reports (`None` = a guest). The
    /// framework renders the chip; the product's callbacks decide what
    /// signing in and out mean (its own dialog, a license import) — the
    /// framework never authenticates.
    account: Option<Account>,
    on_sign_in: Option<Rc<RefCell<dyn Fn()>>>,
    on_sign_out: Option<Rc<RefCell<dyn Fn()>>>,
    /// The script plugins the app layer loaded, as the plugins panel shows
    /// them (`None` = the panel is not registered). Enabled state is
    /// runtime: the panel's disable removes a plugin's panels.
    plugins: Option<Vec<PluginEntry>>,
    /// Plugins that failed to load, with the reason.
    rejected_plugins: Vec<(String, String)>,
    /// The UI locale every string in this shell renders in: a user setting
    /// (`ui.locale`), defaulted from the harness profile.
    locale: Locale,
    /// The settings panel's font-family input, created on first show.
    font_input: Rc<RefCell<Option<Entity<InputState>>>>,
    _font_input_sub: Rc<RefCell<Option<Subscription>>>,
}

/// One script plugin as the plugins panel shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginEntry {
    pub id: String,
    pub enabled: bool,
}

impl<S: SessionView> HarnessShell<S> {
    /// Opens the store's open project at its session index. A project whose
    /// session index names nothing (no sessions yet) opens with an empty
    /// workspace.
    pub fn new(store: SessionStore<S>) -> Self {
        let mut shell: Self = Self {
            store: RefCell::new(store),
            workspace: None,
            viewports: HashMap::new(),
            product_name: "harness".to_string(),
            on_session_activated: None,
            composer: None,
            on_message_sent: None,
            on_plan_decision: None,
            sidebar_projection: None,
            on_undo: None,
            on_redo: None,
            on_save: None,
            on_load: None,
            on_import: None,
            _composer_sub: None,
            selected_entity: None,
            selected_block: None,
            panel_host: PanelHost::default(),
            settings: None,
            theme: ThemeKind::Dark,
            account: None,
            on_sign_in: None,
            on_sign_out: None,
            plugins: None,
            rejected_plugins: Vec::new(),
            locale: Locale::default(),
            font_input: Rc::new(RefCell::new(None)),
            _font_input_sub: Rc::new(RefCell::new(None)),
        };
        // Framework chrome: the diagnostics panel is always there (it reads
        // nothing but shell state), the settings panel joins when the app
        // layer shares a store.
        shell.panel_host.register(PanelDef::product(
            "harness.diagnostics",
            shell.tr("shell.diagnostics"),
            PanelArea::Bottom,
            1,
            Rc::new(diagnostics_panel),
        ));
        let session_id = shell.store.borrow().open_project().session_index.clone();
        if let Some(session_id) = session_id {
            if shell.store.borrow().session(&session_id).is_some() {
                shell.present(&session_id);
            }
        }
        shell
    }

    /// Like [`HarnessShell::new`], but with the message composer wired up. The
    /// composer needs a window to host its input entity, so this is the
    /// windowed constructor; headless callers use [`HarnessShell::new`].
    pub fn with_composer(
        store: SessionStore<S>,
        locale: Locale,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::i18n::t(locale, "composer.placeholder"))
        });
        let subscription = cx.subscribe_in(
            &composer,
            window,
            |shell: &mut Self, input, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::PressEnter { .. }) {
                    return;
                }
                let text = input.read(cx).value().to_string();
                if text.trim().is_empty() {
                    return;
                }
                let Some(session_id) = shell.workspace.as_ref().map(|ws| ws.session_id().clone())
                else {
                    return;
                };
                if let Some(sender) = shell.on_message_sent.clone() {
                    (sender.borrow())(&session_id, &text);
                    input.update(cx, |state, cx| state.set_value("", window, cx));
                }
            },
        );

        let mut shell = Self::new(store);
        shell.locale = locale;
        shell.composer = Some(composer);
        shell._composer_sub = Some(subscription);
        shell
    }

    /// The product's display name in the header (from `harness.toml`).
    pub fn set_product_name(&mut self, name: impl Into<String>) {
        self.product_name = name.into();
    }

    pub fn product_name(&self) -> &str {
        &self.product_name
    }

    pub fn set_on_session_activated<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_session_activated = Some(Rc::new(RefCell::new(callback)));
    }

    pub fn set_on_message_sent<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId, &str) + 'static,
    {
        self.on_message_sent = Some(Rc::new(RefCell::new(callback)));
    }

    pub fn set_on_plan_decision<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId, PlanDecision) + 'static,
    {
        self.on_plan_decision = Some(Rc::new(RefCell::new(callback)));
    }

    /// Supply (or replace) the sidebar's projection. `None` restores the
    /// default flat session list. Callers must notify: the sidebar renders on
    /// the next frame.
    pub fn set_sidebar_projection(
        &mut self,
        projection: Option<Rc<dyn crate::model::SidebarProjection<S>>>,
    ) {
        self.sidebar_projection = projection;
    }

    /// Whether a product projection is active (what a toggle renders).
    pub fn has_sidebar_projection(&self) -> bool {
        self.sidebar_projection.is_some()
    }

    pub fn set_on_undo<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_undo = Some(Rc::new(RefCell::new(callback)));
    }

    pub fn set_on_redo<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_redo = Some(Rc::new(RefCell::new(callback)));
    }

    pub fn set_on_save<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_save = Some(Rc::new(RefCell::new(callback)));
    }

    pub fn set_on_load<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_load = Some(Rc::new(RefCell::new(callback)));
    }

    pub fn set_on_import<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_import = Some(Rc::new(RefCell::new(callback)));
    }

    /// Shares the harness's user settings with the shell. Presence registers
    /// the framework's settings panel (bottom dock: theme choice, settings
    /// file path) and makes theme choices persist through `ui.theme`; the
    /// sidebar's gear button becomes a theme quick-toggle either way.
    /// Registering twice adds nothing.
    pub fn set_settings(&mut self, settings: Option<Arc<Mutex<Settings>>>) {
        if let Some(store) = &settings {
            self.theme =
                store.lock().map(|s| crate::theme::kind_from_settings(&s)).unwrap_or(self.theme);
            if let Some(code) = store
                .lock()
                .ok()
                .and_then(|s| s.get(crate::theme::LOCALE_KEY).cloned())
                .and_then(|v| v.as_str().map(str::to_string))
            {
                self.locale = Locale::from_code(&code);
            }
            const SETTINGS_PANEL_ID: &str = "harness.settings";
            if !self
                .panel_host
                .panels()
                .iter()
                .any(|panel| panel.id == SETTINGS_PANEL_ID)
            {
                self.panel_host.register(PanelDef::product(
                    SETTINGS_PANEL_ID,
                    self.tr("shell.settings"),
                    PanelArea::Bottom,
                    0,
                    Rc::new(settings_panel),
                ));
            }
        }
        self.settings = settings;
    }

    /// The shared settings store, when the app layer provided one.
    pub fn settings(&self) -> Option<Arc<Mutex<Settings>>> {
        self.settings.clone()
    }

    /// The settings file's path as the panels show it (`None` = in-memory,
    /// nothing shared). The single read the settings and diagnostics panels
    /// share.
    pub fn settings_path(&self) -> Option<String> {
        self.settings
            .as_ref()
            .and_then(|store| store.lock().ok())
            .and_then(|settings| settings.path().map(|path| path.display().to_string()))
    }

    /// The theme the shell renders in.
    pub fn theme(&self) -> ThemeKind {
        self.theme
    }

    /// The UI locale. Switch it with [`HarnessShell::set_locale`] (the
    /// settings panel does, through `ui.locale`).
    pub fn locale(&self) -> Locale {
        self.locale
    }

    /// Switches the UI locale; the shell re-renders on the next frame.
    pub fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    /// The settings panel's font input handle (the panel creates it on
    /// first show).
    pub fn font_input(&self) -> Rc<RefCell<Option<Entity<InputState>>>> {
        self.font_input.clone()
    }

    pub(crate) fn font_input_sub(&self) -> Rc<RefCell<Option<Subscription>>> {
        self._font_input_sub.clone()
    }

    /// Writes one font setting and re-themes the window. The settings panel
    /// is the only caller today; the app layer applies the stored fonts at
    /// launch through [`crate::theme::apply_font`].
    fn set_font_setting(
        &mut self,
        key: &'static str,
        value: serde_json::Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(store) = &self.settings {
            if let Ok(mut store) = store.lock() {
                let _ = store.set(key, value);
                crate::theme::apply_font(&store, Some(window), cx);
            }
        }
    }

    /// The localized text for `key` in the shell's locale.
    fn tr(&self, key: &'static str) -> &'static str {
        crate::i18n::t(self.locale, key)
    }

    /// [`tr`](Self::tr) with `{placeholder}` substitution.
    fn trf(&self, key: &str, args: &[(&str, &str)]) -> String {
        crate::i18n::t_replace(self.locale, key, args)
    }

    /// Switches the theme: remembers it on the shell, persists it through
    /// the settings store when one is shared, and applies it to every themed
    /// surface (the global theme change refreshes the window).
    fn switch_theme(&mut self, kind: ThemeKind, window: &mut Window, cx: &mut Context<Self>) {
        self.theme = kind;
        match &self.settings {
            Some(store) => crate::theme::set_theme(store, kind, Some(window), cx),
            None => crate::theme::apply(kind, Some(window), cx),
        }
    }

    /// The identity the sidebar's account chip shows (`None` = a guest).
    /// Feed it from the harness's account provider whenever the app layer
    /// learns the snapshot changed.
    pub fn set_account(&mut self, account: Option<Account>) {
        self.account = account;
    }

    /// The identity the chip currently shows.
    pub fn account(&self) -> Option<&Account> {
        self.account.as_ref()
    }

    /// What the chip's sign-in affordance does (the product opens its own
    /// dialog — cloud sign-in, license import; the framework never
    /// authenticates). Without a callback the affordance is inert.
    pub fn set_on_sign_in<F>(&mut self, callback: F)
    where
        F: Fn() + 'static,
    {
        self.on_sign_in = Some(Rc::new(RefCell::new(callback)));
    }

    /// What the chip's sign-out affordance does.
    pub fn set_on_sign_out<F>(&mut self, callback: F)
    where
        F: Fn() + 'static,
    {
        self.on_sign_out = Some(Rc::new(RefCell::new(callback)));
    }

    /// Shares the plugin load result with the shell: registers the plugins
    /// panel (bottom dock — loaded plugins with a disable button, rejected
    /// ones with the reason). Calling again refreshes the snapshot while
    /// keeping what the user already disabled. A plugin disable at runtime
    /// removes exactly that plugin's panels; commands and toolbar items
    /// have no shell surface yet, so there is nothing else to withdraw.
    pub fn set_plugins(&mut self, loaded: &nanobot_rhai::LoadedPlugins) {
        let disabled: Vec<String> = self
            .plugins
            .take()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| !entry.enabled)
            .map(|entry| entry.id)
            .collect();
        self.plugins = Some(
            loaded
                .plugins_loaded
                .iter()
                .map(|id| PluginEntry {
                    id: id.clone(),
                    enabled: !disabled.contains(id),
                })
                .collect(),
        );
        self.rejected_plugins = loaded.plugins_rejected.clone();

        const PLUGINS_PANEL_ID: &str = "harness.plugins";
        if !self
            .panel_host
            .panels()
            .iter()
            .any(|panel| panel.id == PLUGINS_PANEL_ID)
        {
            self.panel_host.register(PanelDef::product(
                PLUGINS_PANEL_ID,
                self.tr("shell.plugins"),
                PanelArea::Bottom,
                2,
                Rc::new(plugins_panel),
            ));
        }
    }

    /// The plugin snapshot (ids + enabled state), when shared.
    pub fn plugins(&self) -> Option<&[PluginEntry]> {
        self.plugins.as_deref()
    }

    /// Disables a plugin at runtime: removes exactly its panels from the
    /// dock and flips the snapshot. Returns the panels removed.
    pub fn disable_plugin(&mut self, id: &str) -> usize {
        let removed = self.panel_host.remove_plugin(id);
        if let Some(plugins) = &mut self.plugins {
            for entry in plugins.iter_mut() {
                if entry.id == id {
                    entry.enabled = false;
                }
            }
        }
        removed
    }

    pub fn store(&self) -> Ref<'_, SessionStore<S>> {
        self.store.borrow()
    }

    /// Selects an entity by name (three-way linking). Clears the conversation
    /// block selection.
    pub fn select_entity(&mut self, name: Option<String>) {
        self.selected_entity = name;
        self.selected_block = None;
    }

    pub fn selected_entity(&self) -> Option<&str> {
        self.selected_entity.as_deref()
    }

    /// Selects a conversation block by event sequence. Clears the entity
    /// selection.
    pub fn select_block(&mut self, seq: Option<u64>) {
        self.selected_block = seq;
        self.selected_entity = None;
    }

    pub fn selected_block(&self) -> Option<u64> {
        self.selected_block
    }

    /// Sets the geometry calculation progress. `(current, total)` or `None`.
    pub fn set_progress(&mut self, progress: Option<(u32, u32)>) {
        if let Some(ref mut ws) = self.workspace {
            ws.set_progress(progress);
        }
    }

    /// The active session's workspace; `None` while the open project has no
    /// sessions (the UI renders its empty state instead).
    pub fn workspace(&self) -> Option<&S::Workspace> {
        self.workspace.as_ref()
    }

    pub fn active_session_id(&self) -> Option<&SessionId> {
        self.workspace.as_ref().map(|ws| ws.session_id())
    }

    /// Frames the active session's viewport rendered (0 while there is none).
    pub fn painted_frames(&self) -> u64 {
        let Some(id) = self.active_session_id() else {
            return 0;
        };
        self.viewports
            .get(id)
            .map(S::Viewport::painted_frames)
            .unwrap_or(0)
    }

    /// Updates a session's content from data loaded out of the session's event
    /// log. Called by the app layer when a session is activated.
    pub fn update_session_content(&self, id: &SessionId, session: S) -> bool {
        self.store.borrow_mut().update_session_content(id, session)
    }

    /// Hands a freshly baked scene to the active session's renderer.
    pub fn update_scene(&self, scene: &S::Scene) {
        let Some(id) = self.active_session_id() else {
            return;
        };
        let Some(handle) = self.viewports.get(id) else {
            return;
        };
        S::Viewport::update_scene(handle, scene);
    }

    /// The active session's scene projection; the renderer owns the default.
    pub fn viewport_projection(&self) -> Option<ViewportProjection> {
        let id = self.active_session_id()?;
        let handle = self.viewports.get(id)?;
        S::Viewport::projection(handle)
    }

    /// The session's own viewport, created on first use.
    pub fn viewport_of(&mut self, id: &SessionId) -> <S::Viewport as ViewportSlot>::Handle {
        self.viewports
            .entry(id.clone())
            .or_insert_with(S::Viewport::new_handle)
            .clone()
    }

    /// The panel host: the product registers its domain panels here, and the
    /// script host attaches plugin panels.
    pub fn panel_host(&mut self) -> &mut PanelHost<S> {
        &mut self.panel_host
    }

    /// Attaches the panels a script plugin declared.
    pub fn attach_plugins(&mut self, panels: &[nanobot_rhai::PanelRegistration]) {
        self.panel_host.attach_plugins(panels);
    }

    /// The number of registered dock panels (product + script).
    pub fn panel_count(&self) -> usize {
        self.panel_host.len()
    }

    /// Script panels the dock actually mounted in the running window.
    pub fn mounted_script_panels(&self) -> usize {
        self.panel_host.mounted_scripts()
    }

    /// Script panels that failed to mount, with the reason. Non-empty means the
    /// dock rendered an error placeholder instead of the plugin's UI.
    pub fn script_panel_errors(&self) -> Vec<(String, String)> {
        self.panel_host.script_errors()
    }

    /// Builds and installs the workspace of `id` (the whole-replacement step)
    /// and records it as the project's session index.
    fn present(&mut self, id: &SessionId) -> bool {
        let Some(session) = self.store.borrow().session(id).cloned() else {
            return false;
        };
        self.store.borrow_mut().set_session_index(id);
        let viewport = self.viewport_of(id);
        self.workspace = Some(session.build_workspace(viewport));
        if let Some(ref callback) = self.on_session_activated {
            callback.borrow()(id);
        }
        true
    }

    /// Switches the presentation to `id`: the entire workspace is rebuilt from
    /// that session. Returns false if `id` is not a session of the open project,
    /// or is already active.
    pub fn switch_session(&mut self, id: &SessionId) -> bool {
        if self.active_session_id() == Some(id) || self.store.borrow().session(id).is_none() {
            return false;
        }
        self.present(id)
    }

    /// Opens another project: the same whole-workspace replacement as a session
    /// switch, starting at the project's session index.
    pub fn switch_project(&mut self, id: &ProjectId) -> bool {
        if !self.store.borrow_mut().set_open_project(id) {
            return false;
        }
        let session_id = self.store.borrow().open_project().session_index.clone();
        self.workspace = None;
        if let Some(session_id) = session_id {
            if self.store.borrow().session(&session_id).is_some() {
                self.present(&session_id);
            }
        }
        true
    }
}

// ---- palette / small parts ----

/// Color slots the panels use (copied out of the gpui-component theme so the
/// element builders do not hold a borrow of it).
#[derive(Clone, Copy)]
struct Palette {
    fg: Hsla,
    muted: Hsla,
    border: Hsla,
    accent: Hsla,
    success: Hsla,
    info: Hsla,
    warning: Hsla,
    danger: Hsla,
}

fn palette(cx: &App) -> Palette {
    let theme = cx.theme();
    Palette {
        fg: theme.foreground,
        muted: theme.muted_foreground,
        border: theme.border,
        accent: theme.accent,
        success: theme.success,
        info: theme.info,
        warning: theme.warning,
        danger: theme.danger,
    }
}

fn session_color(status: SessionStatus, p: Palette) -> Hsla {
    match status {
        SessionStatus::Active => p.success,
        SessionStatus::Running => p.info,
        SessionStatus::AwaitingReview => p.warning,
        SessionStatus::Archived => p.muted,
    }
}

fn step_color(status: StepStatus, p: Palette) -> Hsla {
    match status {
        StepStatus::Pending => p.muted,
        StepStatus::InProgress => p.info,
        StepStatus::Done => p.success,
    }
}

fn block_color(kind: BlockKind, p: Palette) -> Hsla {
    match kind {
        BlockKind::User => p.fg,
        BlockKind::Assistant => p.info,
        BlockKind::Tool => p.success,
        BlockKind::Plan => p.warning,
        BlockKind::Check => p.success,
    }
}

fn tool_color(state: ToolState, p: Palette) -> Hsla {
    match state {
        ToolState::Running => p.info,
        ToolState::Done => p.success,
        ToolState::Failed => p.danger,
    }
}

/// A tinted pill (session status, tool state, check result).
pub(crate) fn badge(text: &str, color: Hsla) -> impl IntoElement {
    div()
        .px_2()
        .py_1()
        .rounded(px(4.))
        .bg(color.opacity(0.16))
        .text_color(color)
        .text_xs()
        .child(text.to_string())
}

/// A titled panel frame: small caption row plus a padded body.
fn panel(caption: &str, p: Palette, body: impl IntoElement) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .child(
            div()
                .flex_none()
                .px_3()
                .py_1()
                .text_xs()
                .text_color(p.muted)
                .border_b_1()
                .border_color(p.border)
                .child(caption.to_string()),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .overflow_hidden()
                .child(body),
        )
}

// ---- rendering ----

impl<S: SessionView> HarnessShell<S> {
    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let store = self.store.borrow();
        let project = store.open_project();
        let workspace = self.workspace.as_ref();
        let (done, total) = workspace.map(plan_progress_of).unwrap_or((0, 0));
        let session_line = match workspace {
            Some(ws) => ws.title(),
            None => self.tr("shell.no_sessions").to_string(),
        };
        let status = workspace.map(|ws| ws.status());
        div()
            .h(px(46.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .border_b_1()
            .border_color(p.border)
            .child(div().font_semibold().child(self.product_name.clone()))
            .child(
                div()
                    .text_sm()
                    .text_color(p.muted)
                    .child(format!("{} · {}", project.name, session_line)),
            )
            .when_some(status, |d, status| {
                d.child(badge(status.label(self.locale), session_color(status, p)))
            })
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(self.undo_redo_buttons(cx))
                    .child(self.file_ops_buttons(cx)),
            )
            .child(div().text_xs().text_color(p.muted).child(self.trf(
                "shell.viewport_caption",
                &[("done", &done.to_string()), ("total", &total.to_string())],
            )))
    }

    fn undo_redo_buttons(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let session_id = self.workspace.as_ref().map(|ws| ws.session_id().clone());

        let undo_cb = self
            .on_undo
            .clone()
            .unwrap_or_else(|| Rc::new(RefCell::new(|_: &SessionId| {})));
        let sid_undo = session_id.clone().unwrap_or_else(|| SessionId::new(""));
        let undo_btn = div()
            .id("harness-undo")
            .text_sm()
            .px_2()
            .py_1()
            .border_1()
            .border_color(p.border)
            .cursor_pointer()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(
                img(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/undo.svg"))
                    .w(px(14.))
                    .h(px(14.)),
            )
            .child(self.tr("shell.undo"))
            .on_click(move |_, _, _| {
                undo_cb.borrow()(&sid_undo);
            });

        let redo_cb = self
            .on_redo
            .clone()
            .unwrap_or_else(|| Rc::new(RefCell::new(|_: &SessionId| {})));
        let sid_redo = session_id.unwrap_or_else(|| SessionId::new(""));
        let redo_btn = div()
            .id("harness-redo")
            .text_sm()
            .px_2()
            .py_1()
            .border_1()
            .border_color(p.border)
            .cursor_pointer()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(
                img(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/redo.svg"))
                    .w(px(14.))
                    .h(px(14.)),
            )
            .child(self.tr("shell.redo"))
            .on_click(move |_, _, _| {
                redo_cb.borrow()(&sid_redo);
            });

        div()
            .flex()
            .flex_row()
            .gap_2()
            .child(undo_btn)
            .child(redo_btn)
    }

    fn file_ops_buttons(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let sid = self.workspace.as_ref().map(|ws| ws.session_id().clone());

        let sid_save = sid.clone().unwrap_or_else(|| SessionId::new(""));
        let save_cb = self
            .on_save
            .clone()
            .unwrap_or_else(|| Rc::new(RefCell::new(|_: &SessionId| {})));
        let save_btn = header_button("harness-save", self.tr("shell.save"), p.border)
            .on_click(move |_, _, _| {
                save_cb.borrow()(&sid_save);
            });

        let sid_load = sid.clone().unwrap_or_else(|| SessionId::new(""));
        let load_cb = self
            .on_load
            .clone()
            .unwrap_or_else(|| Rc::new(RefCell::new(|_: &SessionId| {})));
        let load_btn = header_button("harness-load", self.tr("shell.load"), p.border)
            .on_click(move |_, _, _| {
                load_cb.borrow()(&sid_load);
            });

        let sid_import = sid.unwrap_or_else(|| SessionId::new(""));
        let import_cb = self
            .on_import
            .clone()
            .unwrap_or_else(|| Rc::new(RefCell::new(|_: &SessionId| {})));
        let import_btn = header_button("harness-import", self.tr("shell.import"), p.border)
            .on_click(move |_, _, _| {
                import_cb.borrow()(&sid_import);
            });

        div()
            .flex()
            .flex_row()
            .gap_2()
            .child(save_btn)
            .child(load_btn)
            .child(import_btn)
    }

    /// The session list (home sidebar): project switcher on top, then the open
    /// project's sessions grouped by project.
    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let mut projects = div().flex().flex_col().gap_1();
        for project in self.store.borrow().projects() {
            let id = project.id.clone();
            let selected = &id == self.store.borrow().open_project_id();
            let sessions = project.sessions.len();
            projects = projects.child(
                div()
                    .px_2()
                    .py_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(p.accent))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                            if this.switch_project(&id) {
                                cx.notify();
                            }
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .when(selected, |d| d.font_semibold())
                                    .child(project.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(p.muted)
                                    .child(format!("{sessions}")),
                            ),
                    ),
            );
        }

        let mut project_tree = div().flex().flex_col().gap_1();
        for project in self.store.borrow().projects() {
            let project_id = project.id.clone();
            let selected_project = &project_id == self.store.borrow().open_project_id();
            let sessions = project.sessions.clone();
            let project_name = project.name.clone();
            let session_count = sessions.len();

            let project_row = div()
                .px_2()
                .py_1()
                .rounded(px(4.))
                .cursor_pointer()
                .when(selected_project, |d| d.bg(p.accent.opacity(0.3)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                        if this.switch_project(&project_id) {
                            cx.notify();
                        }
                    }),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(div().text_xs().text_color(p.info).child("▸".to_string()))
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .when(selected_project, |d| d.font_semibold())
                                .child(project_name),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(p.muted)
                                .child(format!("{session_count}")),
                        ),
                );

            project_tree = project_tree.child(project_row);

            if selected_project {
                for (label, members) in crate::model::resolve_sidebar_groups(
                    &self.store.borrow(),
                    self.sidebar_projection.as_deref(),
                ) {
                    if self.sidebar_projection.is_some() {
                        project_tree = project_tree.child(
                            div()
                                .px_2()
                                .pt_1()
                                .text_xs()
                                .text_color(p.muted)
                                .child(label),
                        );
                    }
                    for session in &members {
                        project_tree = project_tree.child(self.session_row(session, p, cx));
                    }
                }
            }
        }

        div()
            .w(px(SIDEBAR_W))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .border_r_1()
            .border_color(p.border)
            .child(div().text_xs().text_color(p.muted).child(self.tr("shell.projects")))
            .child(projects)
            .child(
                div()
                    .flex_none()
                    .px_2()
                    .pt_2()
                    .text_xs()
                    .text_color(p.muted)
                    .child(self.trf(
                        "shell.sessions",
                        &[
                            ("count", &self.store.borrow().open_project().name),
                            ("total", &self.store.borrow().open_project_sessions().len().to_string()),
                        ],
                    )),
            )
            .child(
                div()
                    .id("harness-session-list".to_string())
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .overflow_y_scroll()
                    .child(project_tree),
            )
            .child(self.sidebar_footer(p, cx))
    }

    fn sidebar_footer(&self, p: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_none()
            .px_2()
            .py_2()
            .border_t_1()
            .border_color(p.border)
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(self.account_chip(p, cx))
            .child(div().flex_1())
            .child(
                div()
                    .id("harness-mobile-toggle")
                    .p_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .on_click(move |_, _, _| {})
                    .child(
                        img(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/mobile.svg"))
                            .w(px(16.))
                            .h(px(16.)),
                    ),
            )
            .child(
                div()
                    .id("harness-settings")
                    .p_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .on_click(cx.listener(move |shell, _event, window, cx| {
                        // The gear is the theme quick-toggle; the settings
                        // panel (bottom dock) is the explicit surface.
                        let next = shell.theme().toggled();
                        shell.switch_theme(next, window, cx);
                    }))
                    .child(
                        img(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/settings.svg"))
                            .w(px(16.))
                            .h(px(16.)),
                    ),
            )
    }

    /// The sidebar footer's account chip: the identity the account provider
    /// reports (avatar letter, name, plan badge) or a guest with a sign-in
    /// affordance. Clicking a signed-in chip signs out. The shell renders
    /// the chip; the product's callbacks decide what either action means.
    fn account_chip(&self, p: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        match self.account.clone() {
            Some(account) => {
                let plan = account.plan.clone();
                let mut chip = div()
                    .id("harness-account")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .cursor_pointer()
                    .on_click(cx.listener(move |shell, _event, _window, _cx| {
                        if let Some(sign_out) = shell.on_sign_out.clone() {
                            (sign_out.borrow())();
                        }
                    }))
                    .child(
                        div()
                            .w(px(24.))
                            .h(px(24.))
                            .rounded(px(12.))
                            .bg(p.accent.opacity(0.3))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(p.accent)
                                    .child(avatar_letter(&account.display_name).to_string()),
                            ),
                    )
                    .child(div().text_xs().min_w_0().child(crate::widgets::truncate(
                        &account.display_name,
                        10,
                    )));
                if let Some(plan) = plan {
                    chip = chip.child(badge(&plan, p.accent));
                }
                chip
            }
            None => div()
                .id("harness-account-guest")
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .on_click(cx.listener(move |shell, _event, _window, _cx| {
                    if let Some(sign_in) = shell.on_sign_in.clone() {
                        (sign_in.borrow())();
                    }
                }))
                .child(
                    div()
                        .w(px(24.))
                        .h(px(24.))
                        .rounded(px(12.))
                        .bg(p.muted.opacity(0.2))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(div().text_xs().text_color(p.muted).child("?".to_string())),
                )
                .child(div().text_xs().text_color(p.muted).child(self.tr("shell.guest_signin"))),
        }
    }

    fn session_row(&self, session: &S, p: Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let id = session.id().clone();
        let selected = self.active_session_id() == Some(&id);
        let (done, total) = plan_progress(&session.plan());
        let color = session_color(session.status(), p);
        let title = session.title();
        let status = session.status();
        let branch = session.branch_marker();
        div()
            .px_2()
            .py_2()
            .rounded(px(6.))
            .cursor_pointer()
            .when(selected, |d| d.bg(p.accent))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                    if this.switch_session(&id) {
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .when_some(branch, |d, _marker| {
                        d.child(div().text_xs().text_color(p.info).child("↧".to_string()))
                    })
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .when(selected, |d| d.font_semibold())
                            .child(title),
                    )
                    .child(badge(status.label(self.locale), color)),
            )
            .child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(p.muted)
                    .child(self.trf("shell.plan_progress", &[("done", &done.to_string()), ("total", &total.to_string())])),
            )
    }

    /// The viewport region: the product renderer's element, fed by this shell's
    /// input forwarding (drag = orbit, Shift+drag = pan, wheel = zoom — the
    /// mapping itself lives in the renderer).
    fn viewport_panel(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .w(px(VIEWPORT_W))
            .flex_none()
            .h_full()
            .border_l_1()
            .border_color(p.border)
            .child(self.viewport_slot(window, cx))
    }

    fn viewport_slot(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(ws) = self.workspace.as_ref() else {
            return div().size_full().into_any_element();
        };
        let id = ws.session_id().clone();
        let Some(handle) = self.viewports.get(&id).cloned() else {
            return div().size_full().into_any_element();
        };
        let (down, moved, up, scroll) = (
            handle.clone(),
            handle.clone(),
            handle.clone(),
            handle.clone(),
        );
        let element = S::Viewport::element(&handle);
        div()
            .id("harness-viewport".to_string())
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_this, ev: &MouseDownEvent, _w, cx| {
                    S::Viewport::feed_input(
                        &down,
                        ViewportInput {
                            mouse_pos: Some(ev.position),
                            mouse_down: true,
                            shift: ev.modifiers.shift,
                            scroll: None,
                        },
                    );
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |_this, ev: &MouseMoveEvent, _w, cx| {
                S::Viewport::feed_input(
                    &moved,
                    ViewportInput {
                        mouse_pos: Some(ev.position),
                        mouse_down: ev.pressed_button == Some(MouseButton::Left),
                        shift: ev.modifiers.shift,
                        scroll: None,
                    },
                );
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |_this, ev: &MouseUpEvent, _w, cx| {
                    S::Viewport::feed_input(
                        &up,
                        ViewportInput {
                            mouse_pos: Some(ev.position),
                            mouse_down: false,
                            shift: ev.modifiers.shift,
                            scroll: None,
                        },
                    );
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(move |_this, ev: &ScrollWheelEvent, _w, cx| {
                // Pixels, or lines scaled like a wheel notch.
                let dy = match ev.delta {
                    ScrollDelta::Pixels(p) => f32::from(p.y),
                    ScrollDelta::Lines(l) => l.y * 40.0,
                };
                S::Viewport::feed_input(
                    &scroll,
                    ViewportInput {
                        mouse_pos: Some(ev.position),
                        mouse_down: false,
                        shift: ev.modifiers.shift,
                        scroll: Some(dy),
                    },
                );
                cx.notify();
            }))
            .child(element)
            .into_any_element()
    }

    /// The session workspace: plan state over the conversation stream.
    fn ai_workspace(&self, ws: &S::Workspace, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let plan = plan_rows(&ws.plan(), p, self.locale);
        let conversation = self.conversation_stream(&ws.conversation().blocks, p, cx);
        div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(p.border)
            .child(div().flex_none().child(panel(self.tr("shell.plan_state"), p, plan)))
            .when_some(ws.progress(), |panel, (done, total)| {
                let pct = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                panel.child(
                    div()
                        .flex_none()
                        .px_2()
                        .py_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(p.muted)
                                .child(self.trf("shell.compute_progress", &[("done", &done.to_string()), ("total", &total.to_string())])),
                        )
                        .child(
                            div()
                                .w_full()
                                .h(px(4.))
                                .rounded(px(2.))
                                .bg(p.border)
                                .child(
                                    div()
                                        .w(px(pct * 200.0))
                                        .h(px(4.))
                                        .rounded(px(2.))
                                        .bg(p.info),
                                ),
                        ),
                )
            })
            .when(ws.plan_phase().awaits_decision(), |panel| {
                if ws.plan_phase() == PlanPhase::Clarify {
                    panel.child(self.clarify_bar(ws, p))
                } else {
                    panel.child(self.plan_decision_bar(ws, p))
                }
            })
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .border_t_1()
                    .border_color(p.border)
                    .child(panel(
                        self.tr("shell.conversation"),
                        p,
                        div()
                            .id("harness-conversation".to_string())
                            .flex()
                            .flex_col()
                            .gap_2()
                            .overflow_y_scroll()
                            .child(conversation),
                    )),
            )
            .when_some(self.composer.clone(), |panel, composer| {
                panel.child(
                    div()
                        .flex_none()
                        .p_2()
                        .border_t_1()
                        .border_color(p.border)
                        .child(Input::new(&composer).w_full()),
                )
            })
    }

    /// The Clarify bar: the agent needs more information before proposing.
    fn clarify_bar(&self, ws: &S::Workspace, p: Palette) -> impl IntoElement {
        let mut bar = div()
            .flex_none()
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .border_t_1()
            .border_color(p.border)
            .child(
                div()
                    .text_xs()
                    .text_color(p.warning)
                    .child(self.tr("shell.clarify_prompt")),
            );

        for (i, question) in ws.clarify_questions().iter().enumerate() {
            bar = bar.child(div().text_sm().text_color(p.fg).child(format!(
                "{}. {}",
                i + 1,
                question
            )));
        }

        bar
    }

    /// The approval bar: the plan failed verification and waits on the user.
    fn plan_decision_bar(&self, ws: &S::Workspace, p: Palette) -> impl IntoElement {
        let mut bar = div()
            .flex_none()
            .p_2()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .border_t_1()
            .border_color(p.border)
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(p.warning)
                    .child(self.trf(
                        "shell.plan_rejected",
                        &[("index", ws.plan_phase().label(self.locale))],
                    )),
            );

        for (label, decision) in [
            (self.tr("shell.accept"), PlanDecision::Accept),
            (self.tr("shell.reject"), PlanDecision::Reject),
            (self.tr("shell.modify"), PlanDecision::Modify),
        ] {
            let callback = self.on_plan_decision.clone();
            let session_id = ws.session_id().clone();
            bar = bar.child(
                div()
                    .id(format!("harness-plan-decision-{label}"))
                    .px_2()
                    .py_1()
                    .rounded(px(4.0))
                    .border_1()
                    .border_color(p.border)
                    .cursor_pointer()
                    .text_xs()
                    .child(label)
                    .on_click(move |_, _, _| {
                        if let Some(callback) = callback.clone() {
                            (callback.borrow())(&session_id, decision);
                        }
                    }),
            );
        }

        // Settlement items the product reports (per-feature accept/reject).
        let settlement = ws.settlement_items();
        if !settlement.is_empty() {
            bar = bar.child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(p.muted)
                    .child(self.tr("shell.itemized")),
            );
            for (name, accepted) in settlement {
                let color = if accepted { p.success } else { p.danger };
                bar = bar.child(
                    div()
                        .id(format!("harness-settlement-{name}"))
                        .px_2()
                        .py_1()
                        .rounded(px(4.0))
                        .border_1()
                        .border_color(color)
                        .cursor_pointer()
                        .text_xs()
                        .text_color(color)
                        .child(format!("{} {}", if accepted { "✓" } else { "✗" }, name))
                        .on_click(move |_, _, _| {}),
                );
            }
        }
        bar
    }

    /// The open project has no sessions yet: the session list is empty and there
    /// is no workspace to present.
    fn empty_workspace(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        div()
            .flex_1()
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(
                div()
                    .text_sm()
                    .child(self.trf("shell.no_sessions_in_project", &[("name", &self.store.borrow().open_project().name)])),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(p.muted)
                    .child(self.tr("shell.sessions_note")),
            )
    }

    /// The conversation stream of the active session.
    fn conversation_stream(
        &self,
        blocks: &[Block],
        p: Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selected_block;
        let mut stream = div().flex().flex_col().gap_2();
        for block in blocks {
            let color = block_color(block.kind, p);
            let anchor = format!("#{}", block.event_seq);
            let is_selected = selected == Some(block.event_seq);
            let seq = block.event_seq;
            let mut head = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().text_xs().text_color(color).child(block.kind.label(self.locale)))
                .child(div().text_xs().text_color(p.muted).child(anchor));
            if let Some(state) = block.tool_state {
                head = head.child(badge(state.label(self.locale), tool_color(state, p)));
            }
            stream = stream.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(is_selected, |d| d.bg(p.accent.opacity(0.2)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                            this.select_block(Some(seq));
                            cx.notify();
                        }),
                    )
                    .child(head)
                    .when_some(block.selection_ref.as_ref(), |d, sel| {
                        d.child(
                            div()
                                .px_2()
                                .py_0p5()
                                .rounded(px(3.))
                                .bg(p.info.opacity(0.16))
                                .text_color(p.info)
                                .text_xs()
                                .cursor_pointer()
                                .child(format!("📍 {sel}")),
                        )
                    })
                    .child(div().text_sm().child(block.text.clone())),
            );
        }
        stream
    }

    /// Renders one dock region: every registered panel of that area, stacked.
    fn dock_region(
        &self,
        area: PanelArea,
        panels: &[PanelDef<S>],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if panels.is_empty() {
            return div().into_any_element();
        }
        let mut column = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .overflow_hidden();
        for panel in panels {
            let body = match &panel.source {
                crate::panels::PanelSource::Product(_) => {
                    self.panel_host.product_element(panel, self, window, cx)
                }
                crate::panels::PanelSource::Script(script) => {
                    self.panel_host.script_element(&panel.id, script, window, cx)
                }
            };
            let p = palette(cx);
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(p.border)
                    .child(
                        div()
                            .flex_none()
                            .px_2()
                            .py_1()
                            .text_xs()
                            .text_color(p.muted)
                            .border_b_1()
                            .border_color(p.border)
                            .child(panel.title.clone()),
                    )
                    .child(div().flex_1().overflow_hidden().child(body)),
            );
        }
        let _ = area;
        column.into_any_element()
    }
}

/// A header icon button (save / load / import).
fn header_button(id: &'static str, label: &'static str, border: Hsla) -> Stateful<Div> {
    div()
        .id(id)
        .text_sm()
        .px_2()
        .py_1()
        .border_1()
        .border_color(border)
        .cursor_pointer()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .child(img(assets::icon(id)).w(px(14.)).h(px(14.)))
        .child(label)
}

/// Asset paths, kept in one place so the header buttons stay declarative.
mod assets {
    pub fn icon(id: &str) -> String {
        let name = match id {
            "harness-save" => "save",
            "harness-load" => "load",
            "harness-import" => "import",
            _ => "settings",
        };
        format!("{}/assets/{name}.svg", env!("CARGO_MANIFEST_DIR"))
    }
}

/// `(done, total)` of a workspace's plan.
fn plan_progress_of<W: WorkspaceView>(ws: &W) -> (usize, usize) {
    plan_progress(&ws.plan())
}

/// The avatar letter the account chip shows: the display name's first
/// character (`U` for the empty name, matching the placeholder the chip
/// replaced).
pub(crate) fn avatar_letter(name: &str) -> char {
    name.trim().chars().next().unwrap_or('U')
}

/// The framework's plugins panel body, registered by
/// [`HarnessShell::set_plugins`]: loaded plugins with a runtime disable,
/// rejected plugins with the reason. The disable removes exactly the
/// plugin's own panels — the trust boundary stays declarative.
fn plugins_panel<S: SessionView>(
    shell: &HarnessShell<S>,
    _workspace: &S::Workspace,
    _window: &mut Window,
    cx: &mut Context<HarnessShell<S>>,
) -> AnyElement {
    let p = palette(cx);
    let mut column = div().flex().flex_col().gap_1().px_1();

    if let Some(plugins) = shell.plugins.clone() {
        for entry in plugins {
            let (badge_text, color) = if entry.enabled {
                (shell.tr("plugins.enabled"), p.success)
            } else {
                (shell.tr("plugins.disabled"), p.muted)
            };
            let id = entry.id.clone();
            let mut row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .min_w_0()
                        .child(crate::widgets::truncate(&id, 36)),
                )
                .child(crate::shell::badge(badge_text, color));
            if entry.enabled {
                row = row.child(crate::widgets::button(
                    format!("harness-plugin-disable-{}", id),
                    shell.tr("plugins.disable"),
                    p.danger,
                    cx,
                    cx.listener(move |shell, _event, _window, _cx| {
                        shell.disable_plugin(&id);
                    }),
                ));
            }
            column = column.child(row);
        }
    } else {
        column = column
            .child(div().text_xs().text_color(p.muted).child(shell.tr("plugins.not_shared")));
    }
    for (plugin, reason) in shell.rejected_plugins.clone() {
        column = column.child(
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(div().text_xs().text_color(p.danger).child(shell.trf(
                    "plugins.rejected",
                    &[("name", &crate::widgets::truncate(&plugin, 36))],
                )))
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted)
                        .child(crate::widgets::truncate(&reason, 64)),
                ),
        );
    }
    column.into_any_element()
}

/// The framework's diagnostics panel body, registered by
/// [`HarnessShell::new`] — framework chrome like workbench-rs's
/// DiagnosticsPanel. It reads nothing but shell state: identity, store
/// shape, panel/script health, viewport liveness, settings wiring.
fn diagnostics_panel<S: SessionView>(
    shell: &HarnessShell<S>,
    _workspace: &S::Workspace,
    _window: &mut Window,
    cx: &mut Context<HarnessShell<S>>,
) -> AnyElement {
    let p = palette(cx);
    let store = shell.store.borrow();
    let rows: Vec<(String, String)> = vec![
        (
            shell.tr("diagnostics.product").to_string(),
            shell.product_name.clone(),
        ),
        (
            shell.tr("diagnostics.theme").to_string(),
            shell.theme().as_str().to_string(),
        ),
        (
            shell.tr("diagnostics.projects").to_string(),
            store.projects().len().to_string(),
        ),
        (
            shell.tr("diagnostics.sessions").to_string(),
            shell.trf(
                "diagnostics.sessions_in_project",
                &[
                    ("count", &store.open_project_sessions().len().to_string()),
                    ("name", &store.open_project().name),
                ],
            ),
        ),
        (
            shell.tr("diagnostics.panels").to_string(),
            shell.panel_host.len().to_string(),
        ),
        (
            shell.tr("diagnostics.script_panels").to_string(),
            shell.trf(
                "diagnostics.mounted_errors",
                &[
                    ("mounted", &shell.panel_host.mounted_scripts().to_string()),
                    ("errors", &shell.panel_host.script_errors().len().to_string()),
                ],
            ),
        ),
        (
            shell.tr("diagnostics.viewport_frames").to_string(),
            shell.painted_frames().to_string(),
        ),
        (
            shell.tr("diagnostics.settings").to_string(),
            shell
                .settings_path()
                .unwrap_or_else(|| shell.tr("diagnostics.settings_unconnected").to_string()),
        ),
        (
            shell.tr("diagnostics.account").to_string(),
            shell
                .account()
                .map(|account| account.display_name.clone())
                .unwrap_or_else(|| shell.tr("diagnostics.guest").to_string()),
        ),
    ];
    div()
        .flex()
        .flex_wrap()
        .gap_x_4()
        .gap_y_1()
        .px_1()
        .children(rows.into_iter().map(|(label, value)| {
            div()
                .flex()
                .flex_row()
                .gap_1()
                .child(div().text_xs().text_color(p.muted).child(label.to_string()))
                .child(
                    div()
                        .text_xs()
                        .min_w_0()
                        .child(crate::widgets::truncate(&value, 48)),
                )
                .into_any_element()
        }))
        .into_any_element()
}

/// The framework's settings panel body, registered by
/// [`HarnessShell::set_settings`]: the theme choice plus the settings file
/// path. The dock region supplies the captioned chrome around it.
fn settings_panel<S: SessionView>(
    shell: &HarnessShell<S>,
    _workspace: &S::Workspace,
    window: &mut Window,
    cx: &mut Context<HarnessShell<S>>,
) -> AnyElement {
    let p = palette(cx);
    let current = shell.theme();
    let path = shell.settings_path();

    let theme_button =
        |kind: ThemeKind, label: &'static str, cx: &mut Context<HarnessShell<S>>| {
            let color = if current == kind { p.accent } else { p.muted };
            crate::widgets::button(
                format!("harness-theme-{}", kind.as_str()),
                label,
                color,
                cx,
                cx.listener(move |shell, _event, window, cx| {
                    shell.switch_theme(kind, window, cx);
                }),
            )
        };

    let locale_button = |locale: Locale,
                         label: &'static str,
                         cx: &mut Context<HarnessShell<S>>| {
        let active = shell.locale() == locale;
        let color = if active { p.accent } else { p.muted };
        crate::widgets::button(
            format!("harness-locale-{}", locale.code()),
            label,
            color,
            cx,
            cx.listener(move |shell, _event, _window, _cx| {
                shell.set_locale(locale);
                if let Some(store) = shell.settings() {
                    if let Ok(mut store) = store.lock() {
                        let _ = store.set(crate::theme::LOCALE_KEY, serde_json::json!(locale.code()));
                    }
                }
            }),
        )
    };

    let size_button = |size: f64, label: &'static str, cx: &mut Context<HarnessShell<S>>| {
        let current = shell.settings().and_then(|store| {
            store
                .lock()
                .ok()
                .and_then(|store| store.get(crate::theme::FONT_SIZE_KEY).and_then(|v| v.as_f64()))
        });
        let active = current
            .map(|current| (current - size).abs() < f64::EPSILON)
            .unwrap_or((size - 16.0).abs() < f64::EPSILON);
        let color = if active { p.accent } else { p.muted };
        crate::widgets::button(
            format!("harness-font-size-{size}"),
            label,
            color,
            cx,
            cx.listener(move |shell, _event, window, cx| {
                shell.set_font_setting(
                    crate::theme::FONT_SIZE_KEY,
                    serde_json::json!(size),
                    window,
                    cx,
                );
            }),
        )
    };

    // The font family input is created on first show; Enter applies it.
    if shell.font_input().borrow().is_none() {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::i18n::t(shell.locale(), "settings.font.placeholder"))
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |shell: &mut HarnessShell<S>, input, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::PressEnter { .. }) {
                    return;
                }
                let family = input.read(cx).value().to_string();
                if !family.trim().is_empty() {
                    shell.set_font_setting(
                        crate::theme::FONT_FAMILY_KEY,
                        serde_json::json!(family),
                        window,
                        cx,
                    );
                }
            },
        );
        *shell.font_input().borrow_mut() = Some(input);
        *shell.font_input_sub().borrow_mut() = Some(subscription);
    }

    div()
        .flex()
        .flex_col()
        .gap_1()
        .px_1()
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().text_xs().text_color(p.muted).child(shell.tr("settings.theme")))
                .child(theme_button(ThemeKind::Dark, shell.tr("settings.theme_dark"), cx))
                .child(theme_button(ThemeKind::Light, shell.tr("settings.theme_light"), cx)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().text_xs().text_color(p.muted).child(shell.tr("settings.locale")))
                .child(locale_button(
                    Locale::ZhCn,
                    crate::i18n::t(Locale::ZhCn, "settings.locale.zh"),
                    cx,
                ))
                .child(locale_button(
                    Locale::En,
                    crate::i18n::t(Locale::En, "settings.locale.en"),
                    cx,
                )),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().text_xs().text_color(p.muted).child(shell.tr("settings.font")))
                .child(size_button(14.0, shell.tr("settings.font.small"), cx))
                .child(size_button(16.0, shell.tr("settings.font.normal"), cx))
                .child(size_button(18.0, shell.tr("settings.font.large"), cx)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(div().text_xs().text_color(p.muted).child(shell.tr("settings.font.family")))
                .child(
                    div()
                        .w(px(180.))
                        .when(shell.font_input().borrow().is_some(), |row| {
                            row.child(
                                Input::new(&shell.font_input().borrow().clone().expect("checked above"))
                                    .w_full()
                                    .text_xs(),
                            )
                        }),
                ),
        )
        .child(div().flex_1())
        .child(div().text_xs().text_color(p.muted).child(
            crate::widgets::truncate(
                &path.unwrap_or_else(|| shell.tr("settings.memory_mode").to_string()),
                64,
            ),
        ))
        .into_any_element()
}


fn plan_rows(plan: &[PlanStep], p: Palette, locale: Locale) -> impl IntoElement {
    let mut rows = div().flex().flex_col().gap_1();
    for step in plan {
        let color = step_color(step.status, p);
        rows = rows.child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(8.))
                        .h(px(8.))
                        .flex_none()
                        .rounded(px(4.))
                        .bg(color),
                )
                .child(div().flex_1().text_sm().child(step.text.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted)
                        .child(step.status.label(locale)),
                ),
        );
    }
    rows
}

impl<S: SessionView> Render for HarnessShell<S> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        // The registry is cloned out so the panel factories can be called with
        // an immutable borrow of the shell.
        let panels = self.panel_host.panels().to_vec();
        let left = self.dock_region(PanelArea::Left, &panels, window, cx);
        let right = self.dock_region(PanelArea::Right, &panels, window, cx);
        let top = self.dock_region(PanelArea::Top, &panels, window, cx);
        let bottom = self.dock_region(PanelArea::Bottom, &panels, window, cx);

        let body = match self.workspace.as_ref() {
            Some(ws) => {
                let ai = self.ai_workspace(ws, cx).into_any_element();
                let viewport = self.viewport_panel(window, cx).into_any_element();
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .child(self.sidebar(cx))
                    .child(ai)
                    .child(viewport)
                    .child(right)
                    .into_any_element()
            }
            None => div()
                .flex_1()
                .flex()
                .flex_row()
                .overflow_hidden()
                .child(self.sidebar(cx))
                .child(self.empty_workspace(cx))
                .child(right)
                .into_any_element(),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(p.fg)
            .child(self.header(cx))
            .child(top)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .overflow_hidden()
                    .child(left)
                    .child(body),
            )
            .child(bottom)
    }
}
