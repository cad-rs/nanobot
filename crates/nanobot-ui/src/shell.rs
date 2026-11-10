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
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui::{
    actions, FocusHandle, KeyBinding, MouseMoveEvent, MouseUpEvent,
    div, prelude::*, px, AnyElement, App, Context, Div, Entity, Hsla, Stateful,
    IntoElement,
    MouseButton, MouseDownEvent, Render, Subscription, Window,
};
use gpui_component::{
    input::{Input, InputEvent, InputState, TextareaState},
    ActiveTheme as _, StyledExt as _,
};
use nanobot_boot::account::Account;
use nanobot_boot::Settings;
use nanobot_gpu::{ViewportProjection, ViewportSlot};

use crate::model::{
    plan_progress, PlanDecision, ProjectId,
    SessionId, SessionStatus, SessionStore, SessionView, WorkspaceView,
};
use crate::panels::{PanelArea, PanelDef, PanelHost};
use crate::composer::{ApprovalDecision, Delivery, MessageQueue, PermissionMode};
use crate::i18n::Locale;
use crate::icons::{self, sized, Icon};
use crate::layout::{Divider, Fold, Layout, LayoutState, Stacked};
use crate::stream::StreamState;
use crate::theme::ThemeKind;
use crate::transcript::{SessionBadge, Transcript, TurnPhase};




/// A divider drag in progress: which divider, where the pointer was when it
/// began, and how wide the divider's region was then.
#[derive(Clone, Copy, Debug)]
struct DividerDrag {
    divider: Divider,
    start_x: f32,
    start_width: f32,
}

/// Height of the session column's toolbar.
const TOOLBAR_H: f32 = 36.0;
/// Width of the strip a divider is grabbed by.
const DIVIDER_W: f32 = 5.0;

actions!(
    nanobot_ui,
    [
        /// Shows or hides the session sidebar.
        ToggleSidebar,
        /// Shows or hides the right dock.
        ToggleDock,
        /// Turns the permission dial one step.
        CyclePermissionMode,
        /// Stops the turn in progress.
        StopTurn,
    ]
);

/// The key context the shell's bindings live in.
pub const KEY_CONTEXT: &str = "HarnessShell";

/// Installs the shell's default key bindings. The product calls this once at
/// startup (after `gpui_component::init`); bindings it adds afterwards win.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-alt-j", ToggleSidebar, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-alt-b", ToggleDock, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-tab", CyclePermissionMode, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", StopTurn, Some(KEY_CONTEXT)),
    ]);
}

/// A shell callback keyed by session (activate / undo / redo / save / load /
/// import all share the shape).
type SessionCallback = Rc<RefCell<dyn Fn(&SessionId)>>;

/// The composer's submit callback.
type MessageCallback = Rc<RefCell<dyn Fn(&SessionId, &str, Delivery)>>;

/// The answer to a tool call waiting for approval: the session, the call's
/// event sequence and the decision.
type ApprovalCallback = Rc<RefCell<dyn Fn(&SessionId, u64, ApprovalDecision)>>;

/// Told when the user turns the permission dial.
type PermissionModeCallback = Rc<RefCell<dyn Fn(PermissionMode)>>;

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
    pub(crate) composer: Option<Entity<TextareaState>>,
    /// Whether the composer holds anything to send: what its button shows.
    pub(crate) composer_has_text: bool,
    /// Callback invoked when the user submits the composer.
    pub(crate) on_message_sent: Option<MessageCallback>,
    /// Callback invoked when the user stops the turn in progress.
    pub(crate) on_stop: Option<SessionCallback>,
    /// Callback invoked when the user answers a tool call waiting for approval.
    pub(crate) on_approval: Option<ApprovalCallback>,
    /// Callback invoked when the user turns the permission dial.
    pub(crate) on_permission_mode: Option<PermissionModeCallback>,
    /// How much the agent may do unasked.
    pub(crate) permission_mode: PermissionMode,
    /// Messages held until a session's turn ends, per session.
    pub(crate) queues: HashMap<SessionId, MessageQueue>,
    /// What is unfolded in the activity bar.
    pub(crate) plan_open: bool,
    pub(crate) queue_open: bool,
    /// "Always allow" was pressed and is asking how far it should reach.
    pub(crate) always_scope_open: bool,
    /// Callback invoked when the user decides a contingent plan.
    pub(crate) on_plan_decision: Option<PlanDecisionCallback>,
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
    pub(crate) settings: Option<Arc<Mutex<Settings>>>,
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
    pub(crate) locale: Locale,
    /// The settings panel's font-family input, created on first show.
    font_input: Rc<RefCell<Option<Entity<InputState>>>>,
    _font_input_sub: Rc<RefCell<Option<Subscription>>>,
    /// How the window is divided: the saved preference and what the window's
    /// width does to it now.
    layout: LayoutState,
    /// The divider being dragged, with where the drag began.
    dragging: Option<DividerDrag>,
    /// Where the keyboard rests when nothing else holds the focus, so the
    /// shell's key bindings work before anything was clicked.
    focus_anchor: Option<FocusHandle>,
    /// The active session's conversation as it is on screen, kept so a new
    /// projection reduces to changes at entry indices.
    transcript: Transcript,
    /// The conversation as rows of a virtualised list.
    pub(crate) stream: StreamState,
    /// Sessions that finished a turn while another was on screen.
    unread: RefCell<HashSet<SessionId>>,
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
            composer_has_text: false,
            on_stop: None,
            on_approval: None,
            on_permission_mode: None,
            permission_mode: PermissionMode::default(),
            queues: HashMap::new(),
            plan_open: false,
            queue_open: true,
            always_scope_open: false,
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
            layout: LayoutState::default(),
            dragging: None,
            focus_anchor: None,
            transcript: Transcript::default(),
            stream: StreamState::default(),
            unread: RefCell::new(HashSet::new()),
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
        // Several lines, growing with what is typed. Enter submits and
        // Shift+Enter breaks the line.
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 8)
                .submit_on_enter(true)
                .placeholder(crate::i18n::t(locale, "composer.placeholder"))
        });
        let subscription = cx.subscribe_in(
            &composer,
            window,
            |shell: &mut Self, input, event: &InputEvent, window, cx| match event {
                // With the secondary modifier (Ctrl, or Cmd on macOS) the
                // message goes into the running turn instead of the queue.
                InputEvent::PressEnter {
                    secondary,
                    shift: false,
                } => shell.submit_composer(*secondary, window, cx),
                // The button changes with whether there is anything to send.
                InputEvent::Change => {
                    let has_text = !input.read(cx).value().trim().is_empty();
                    if shell.composer_has_text != has_text {
                        shell.composer_has_text = has_text;
                        cx.notify();
                    }
                }
                _ => {}
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
        F: Fn(&SessionId, &str, Delivery) + 'static,
    {
        self.on_message_sent = Some(Rc::new(RefCell::new(callback)));
    }

    /// What stopping the turn in progress does (the composer's button while
    /// the agent works, or Escape).
    pub fn set_on_stop<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_stop = Some(Rc::new(RefCell::new(callback)));
    }

    /// What answering a tool call that waits for approval does: the session,
    /// the call's event sequence and the user's decision.
    pub fn set_on_approval<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId, u64, ApprovalDecision) + 'static,
    {
        self.on_approval = Some(Rc::new(RefCell::new(callback)));
    }

    /// What turning the permission dial does. The shell shows and remembers
    /// the mode; what each mode lets the agent do is the product's policy.
    pub fn set_on_permission_mode<F>(&mut self, callback: F)
    where
        F: Fn(PermissionMode) + 'static,
    {
        self.on_permission_mode = Some(Rc::new(RefCell::new(callback)));
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
            if let Ok(store) = store.lock() {
                self.layout = LayoutState::new(Layout::load(&store));
                self.permission_mode = PermissionMode::from_code(
                    &store.get_str(crate::activity::PERMISSION_MODE_KEY, ""),
                );
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
    pub(crate) fn tr(&self, key: &'static str) -> &'static str {
        crate::i18n::t(self.locale, key)
    }

    /// [`tr`](Self::tr) with `{placeholder}` substitution.
    pub(crate) fn trf(&self, key: &str, args: &[(&str, &str)]) -> String {
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
        // A turn that ends while the user is looking elsewhere leaves a mark
        // in the sidebar until they come and see it.
        let was_running = self
            .store
            .borrow()
            .session(id)
            .is_some_and(|old| old.status() == SessionStatus::Running);
        let finished = was_running && session.status() == SessionStatus::Idle;
        if finished && self.active_session_id() != Some(id) {
            self.unread.borrow_mut().insert(id.clone());
        }
        self.store.borrow_mut().update_session_content(id, session)
    }

    /// The mark `session` carries in the sidebar.
    pub fn badge_of(&self, session: &S) -> SessionBadge {
        SessionBadge::of(
            session.status(),
            self.unread.borrow().contains(session.id()),
        )
    }

    /// Where the turn of the session on screen stands (`Idle` with none).
    pub fn turn_phase(&self) -> TurnPhase {
        self.workspace
            .as_ref()
            .map_or(TurnPhase::Idle, |ws| ws.turn_phase())
    }

    /// The conversation on screen.
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// The conversation stream: its rows, and what is folded.
    pub fn stream(&self) -> &StreamState {
        &self.stream
    }

    pub fn stream_mut(&mut self) -> &mut StreamState {
        &mut self.stream
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
        let workspace = session.build_workspace(viewport);
        self.transcript.reconcile(workspace.conversation());
        self.stream.set_entries(self.transcript.entries());
        self.workspace = Some(workspace);
        // On screen now: it is no longer news.
        self.unread.borrow_mut().remove(id);
        if let Some(ref callback) = self.on_session_activated {
            callback.borrow()(id);
        }
        true
    }

    /// Rebuilds the workspace on screen from its session in the store: for a
    /// product whose workspace is a snapshot, after
    /// [`update_session_content`](Self::update_session_content). (A workspace
    /// that shares its session's state needs no refresh.) False with no
    /// session on screen.
    pub fn refresh_workspace(&mut self) -> bool {
        let Some(id) = self.active_session_id().cloned() else {
            return false;
        };
        let Some(session) = self.store.borrow().session(&id).cloned() else {
            return false;
        };
        let viewport = self.viewport_of(&id);
        let workspace = session.build_workspace(viewport);
        self.transcript.reconcile(workspace.conversation());
        self.stream.set_entries(self.transcript.entries());
        self.workspace = Some(workspace);
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
pub(crate) struct Palette {
    pub(crate) fg: Hsla,
    pub(crate) muted: Hsla,
    pub(crate) border: Hsla,
    pub(crate) accent: Hsla,
    pub(crate) success: Hsla,
    pub(crate) info: Hsla,
    pub(crate) warning: Hsla,
    pub(crate) danger: Hsla,
}

pub(crate) fn palette(cx: &App) -> Palette {
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
        SessionStatus::Idle => p.muted,
        SessionStatus::Running => p.info,
        SessionStatus::AwaitingUser => p.warning,
        SessionStatus::Error => p.danger,
    }
}

/// The mark a session row carries, if any: its words and its colour.
fn badge_mark(badge: SessionBadge, locale: Locale, p: Palette) -> Option<(&'static str, Hsla)> {
    match badge {
        SessionBadge::AwaitingUser => Some((
            SessionStatus::AwaitingUser.label(locale),
            session_color(SessionStatus::AwaitingUser, p),
        )),
        SessionBadge::Error => Some((
            SessionStatus::Error.label(locale),
            session_color(SessionStatus::Error, p),
        )),
        SessionBadge::Running => Some((
            SessionStatus::Running.label(locale),
            session_color(SessionStatus::Running, p),
        )),
        SessionBadge::Unread => Some((crate::i18n::t(locale, "status.unread"), p.accent)),
        SessionBadge::None => None,
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


// ---- rendering ----

impl<S: SessionView> HarnessShell<S> {
    // ---- layout ----

    /// Whether any panel the user may see is registered for the right dock.
    fn has_right_dock(&self) -> bool {
        !self.panel_host.panels_in(PanelArea::Right).is_empty()
    }

    /// The saved layout preference (sidebar and dock open, the three widths).
    pub fn layout(&self) -> Layout {
        self.layout.layout()
    }

    /// Writes the layout preference through the settings store, when the app
    /// layer shared one. A failed write loses the preference, never the UI.
    fn save_layout(&self) {
        if let Some(Ok(mut settings)) = self.settings.as_ref().map(|s| s.lock()) {
            let _ = self.layout.layout().store(&mut settings);
        }
    }

    /// Shows or hides the session sidebar in a window `window_width` wide.
    pub fn toggle_sidebar(&mut self, window_width: f32) {
        if self.layout.toggle_sidebar(window_width) {
            self.save_layout();
        }
    }

    /// Shows or hides the right dock in a window `window_width` wide.
    pub fn toggle_dock(&mut self, window_width: f32) {
        if self.layout.toggle_dock(window_width) {
            self.save_layout();
        }
    }

    /// Whether the session sidebar shows in a window `window_width` wide.
    pub fn sidebar_shown(&self, window_width: f32) -> bool {
        self.layout
            .regions(window_width, self.has_right_dock())
            .sidebar
            .is_some()
    }

    /// How many sessions other than the one on screen are waiting for the
    /// user: what the toolbar badge counts while the sidebar may be hidden.
    pub fn attention_count(&self) -> usize {
        let active = self.active_session_id();
        self.store
            .borrow()
            .open_project_sessions()
            .iter()
            .filter(|session| Some(session.id()) != active)
            .filter(|session| !session.archived())
            .filter(|session| self.badge_of(session).needs_attention())
            .count()
    }

    fn end_drag(&mut self) {
        if self.dragging.take().is_some() {
            nanobot_gpu::suspend_viewport_input(false);
            self.save_layout();
        }
    }

    /// The strip between two regions that a drag resizes them by.
    fn divider(&self, divider: Divider, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let active = self.dragging.is_some_and(|drag| drag.divider == divider);
        div()
            .id(match divider {
                Divider::Sidebar => "harness-divider-sidebar",
                Divider::Column => "harness-divider-column",
                Divider::Dock => "harness-divider-dock",
            })
            .w(px(DIVIDER_W))
            .h_full()
            .flex_none()
            .cursor_col_resize()
            .border_l_1()
            .border_color(p.border)
            .when(active, |d| d.bg(p.accent.opacity(0.5)))
            .hover(|d| d.bg(p.accent.opacity(0.3)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this: &mut Self, ev: &MouseDownEvent, window, cx| {
                    let width = f32::from(window.viewport_size().width);
                    let Some(start_width) =
                        this.layout.width_at(divider, width, this.has_right_dock())
                    else {
                        return;
                    };
                    this.dragging = Some(DividerDrag {
                        divider,
                        start_x: f32::from(ev.position.x),
                        start_width,
                    });
                    // The pointer will cross the viewport: on Windows its
                    // native surface would otherwise keep the moves and the
                    // release to itself, and the drag would stall at its edge.
                    nanobot_gpu::suspend_viewport_input(true);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
    }

    /// The count of sessions waiting for the user, as the toolbar shows it.
    fn attention_badge(&self, waiting: usize, p: Palette) -> Stateful<Div> {
        div()
            .id("harness-attention")
            .px_2()
            .rounded(px(9.))
            .bg(p.warning.opacity(0.18))
            .text_color(p.warning)
            .text_xs()
            .child(self.trf("shell.attention", &[("count", &waiting.to_string())]))
    }

    /// The session column toolbar: what belongs to the session on screen, plus
    /// the two things that must stay reachable while the sidebar is hidden —
    /// its toggle and the count of sessions waiting for the user.
    fn toolbar(&self, fold: Fold, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let waiting = self.attention_count();
        let workspace = self.workspace.as_ref();
        let title = workspace
            .map_or_else(|| self.tr("shell.no_sessions").to_string(), |ws| ws.title());
        let status = workspace.map(|ws| ws.status());
        div()
            .h(px(TOOLBAR_H))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .border_b_1()
            .border_color(p.border)
            .when(fold != Fold::Stacked, |d| {
                d.child(toolbar_button("harness-toggle-sidebar", Icon::Sidebar, p.muted).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                        this.toggle_sidebar(f32::from(window.viewport_size().width));
                        cx.notify();
                    }),
                ))
            })
            .when(waiting > 0, |d| {
                d.child(self.attention_badge(waiting, p).cursor_pointer().on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                        let width = f32::from(window.viewport_size().width);
                        if this.layout.reveal_sidebar(width) {
                            this.save_layout();
                        }
                        cx.notify();
                    }),
                ))
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .font_semibold()
                    .truncate()
                    .child(title),
            )
            .when_some(status, |d, status| {
                d.child(badge(status.label(self.locale), session_color(status, p)))
            })
            .when(fold == Fold::Stacked, |d| {
                // Too narrow for both: the one way to reach the viewport.
                d.child(
                    text_button("harness-show-viewport", self.tr("shell.show_viewport"), p)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                                this.layout.set_stacked(Stacked::Viewport);
                                cx.notify();
                            }),
                        ),
                )
            })
    }

    /// The strip over a viewport that has the window to itself (a stacked
    /// layout): the way back to the session column.
    fn viewport_return_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let waiting = self.attention_count();
        div()
            .h(px(TOOLBAR_H))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .border_b_1()
            .border_color(p.border)
            .child(
                text_button("harness-show-column", self.tr("shell.show_column"), p).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                        this.layout.set_stacked(Stacked::Column);
                        cx.notify();
                    }),
                ),
            )
            .when(waiting > 0, |d| d.child(self.attention_badge(waiting, p)))
    }

    /// The title bar: only what is global — the product, the open project and
    /// the operations on it. What belongs to one session is in the session
    /// column's toolbar.
    fn header(&self, fold: Fold, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let project = self.store.borrow().open_project().name.clone();
        let has_dock = self.has_right_dock();
        div()
            .h(px(40.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_3()
            .border_b_1()
            .border_color(p.border)
            .child(div().font_semibold().child(self.product_name.clone()))
            .child(div().text_sm().text_color(p.muted).child(project))
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(self.undo_redo_buttons(cx))
                    .child(self.file_ops_buttons(cx)),
            )
            .when(has_dock && fold != Fold::Stacked, |d| {
                d.child(toolbar_button("harness-toggle-dock", Icon::Dock, p.muted).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                        this.toggle_dock(f32::from(window.viewport_size().width));
                        cx.notify();
                    }),
                ))
            })
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
                icons::icon(Icon::Undo, p.fg),
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
                icons::icon(Icon::Redo, p.fg),
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
        let save_btn = header_button("harness-save", Icon::Save, self.tr("shell.save"), p.fg, p.border)
            .on_click(move |_, _, _| {
                save_cb.borrow()(&sid_save);
            });

        let sid_load = sid.clone().unwrap_or_else(|| SessionId::new(""));
        let load_cb = self
            .on_load
            .clone()
            .unwrap_or_else(|| Rc::new(RefCell::new(|_: &SessionId| {})));
        let load_btn = header_button("harness-load", Icon::Load, self.tr("shell.load"), p.fg, p.border)
            .on_click(move |_, _, _| {
                load_cb.borrow()(&sid_load);
            });

        let sid_import = sid.unwrap_or_else(|| SessionId::new(""));
        let import_cb = self
            .on_import
            .clone()
            .unwrap_or_else(|| Rc::new(RefCell::new(|_: &SessionId| {})));
        let import_btn = header_button("harness-import", Icon::Import, self.tr("shell.import"), p.fg, p.border)
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
    fn sidebar(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
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
            .w(px(width))
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
                        sized(Icon::Settings, 16., p.muted),
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
        let mark = badge_mark(self.badge_of(session), self.locale, p);
        let title = session.title();
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
                        d.child(icons::icon(Icon::Branch, p.info))
                    })
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .when(selected, |d| d.font_semibold())
                            .child(title),
                    )
                    .when_some(mark, |d, (label, color)| d.child(badge(label, color))),
            )
            .child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(p.muted)
                    .child(self.trf("shell.plan_progress", &[("done", &done.to_string()), ("total", &total.to_string())])),
            )
    }


    fn viewport_slot(&self) -> impl IntoElement {
        let Some(ws) = self.workspace.as_ref() else {
            return div().size_full().into_any_element();
        };
        let id = ws.session_id().clone();
        let Some(handle) = self.viewports.get(&id).cloned() else {
            return div().size_full().into_any_element();
        };
        // No input listeners here: the renderer's element routes pointer and
        // wheel input itself (over a native surface GPUI's listeners never
        // see it).
        div()
            .id("harness-viewport".to_string())
            .size_full()
            .child(S::Viewport::element(&handle))
            .into_any_element()
    }

    /// The session workspace, top to bottom: the conversation, the activity
    /// bar, the composer.
    fn ai_workspace(&self, ws: &S::Workspace, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_stream(cx))
            .child(self.activity_bar(ws, cx))
            .child(self.composer_area(ws, cx))
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

    /// Renders one dock region: every registered panel of that area, stacked.
    fn dock_region(
        &self,
        area: PanelArea,
        panels: &[PanelDef<S>],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Only this region's panels, in their order.
        let mut panels: Vec<&PanelDef<S>> = panels.iter().filter(|p| p.area == area).collect();
        panels.sort_by_key(|p| p.order);
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
        column.into_any_element()
    }
}

/// A header icon button (save / load / import).
fn header_button(
    id: &'static str,
    icon: Icon,
    label: &'static str,
    color: Hsla,
    border: Hsla,
) -> Stateful<Div> {
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
        .child(icons::icon(icon, color))
        .child(label)
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


impl<S: SessionView> Render for HarnessShell<S> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        // Bring the conversation on screen up to the workspace's projection:
        // what changed reaches the list as rows to remeasure and a splice, so
        // a streaming answer costs its last row, not the whole stream.
        let conversation = self
            .workspace
            .as_ref()
            .map(|ws| ws.conversation())
            .unwrap_or_default();
        if !self.transcript.reconcile(conversation).is_empty() {
            self.stream.set_entries(self.transcript.entries());
        }
        // A session that has gone idle gets the next message it was holding.
        self.drain_queues();
        let window_width = f32::from(window.viewport_size().width);
        self.layout.observe(window_width);
        let regions = self.layout.regions(window_width, self.has_right_dock());

        // Where the keyboard rests when nothing else holds the focus: without
        // it GPUI has no focus path to send the shell's key bindings along
        // until something is clicked.
        let anchor = self
            .focus_anchor
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        if window.focused(cx).is_none() {
            window.focus(&anchor, cx);
        }

        // The registry is cloned out so the panel factories can be called with
        // an immutable borrow of the shell.
        let panels = self.panel_host.visible_panels();
        let left = self.dock_region(PanelArea::Left, &panels, window, cx);
        let right = self.dock_region(PanelArea::Right, &panels, window, cx);
        let top = self.dock_region(PanelArea::Top, &panels, window, cx);
        let bottom = self.dock_region(PanelArea::Bottom, &panels, window, cx);

        let mut body = div().flex_1().flex().flex_row().overflow_hidden().child(left);
        if let Some(width) = regions.sidebar {
            body = body
                .child(self.sidebar(width, cx))
                .child(self.divider(Divider::Sidebar, cx));
        }
        match self.workspace.as_ref() {
            Some(ws) => {
                if let Some(width) = regions.column {
                    // Alone in a stacked layout the column takes the window.
                    let alone = regions.viewport.is_none();
                    body = body.child(
                        div()
                            .h_full()
                            .flex()
                            .flex_col()
                            .overflow_hidden()
                            .map(|d| {
                                if alone {
                                    d.flex_1().min_w_0()
                                } else {
                                    d.w(px(width)).flex_none()
                                }
                            })
                            .child(self.toolbar(regions.fold, cx))
                            .child(self.ai_workspace(ws, cx)),
                    );
                }
                if regions.column.is_some() && regions.viewport.is_some() {
                    body = body.child(self.divider(Divider::Column, cx));
                }
                if regions.viewport.is_some() {
                    // The main area: it takes whatever the others leave.
                    body = body.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .flex()
                            .flex_col()
                            .when(regions.column.is_none(), |d| {
                                d.child(self.viewport_return_bar(cx))
                            })
                            .child(div().flex_1().min_h_0().child(self.viewport_slot())),
                    );
                }
            }
            None => {
                body = body.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .flex_col()
                        .child(self.toolbar(regions.fold, cx))
                        .child(self.empty_workspace(cx)),
                );
            }
        }
        if let Some(width) = regions.dock {
            body = body
                .child(self.divider(Divider::Dock, cx))
                .child(div().w(px(width)).flex_none().h_full().overflow_hidden().child(right));
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().background)
            .text_color(p.fg)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this: &mut Self, _: &ToggleSidebar, window, cx| {
                this.toggle_sidebar(f32::from(window.viewport_size().width));
                cx.notify();
            }))
            .on_action(cx.listener(|this: &mut Self, _: &ToggleDock, window, cx| {
                this.toggle_dock(f32::from(window.viewport_size().width));
                cx.notify();
            }))
            .on_action(cx.listener(|this: &mut Self, _: &CyclePermissionMode, _window, cx| {
                this.cycle_permission_mode();
                cx.notify();
            }))
            .on_action(cx.listener(|this: &mut Self, _: &StopTurn, _window, cx| {
                // Escape with nothing running is for whatever else wants it.
                if this.stop_turn() {
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            // A divider drag is followed here, at the root, so it keeps going
            // wherever the pointer is.
            .on_mouse_move(cx.listener(|this: &mut Self, ev: &MouseMoveEvent, window, cx| {
                let Some(drag) = this.dragging else {
                    return;
                };
                if !ev.dragging() {
                    // The button came up somewhere that did not tell us.
                    this.end_drag();
                    cx.notify();
                    return;
                }
                let moved = f32::from(ev.position.x) - drag.start_x;
                // The dock grows as its divider moves left.
                let width = match drag.divider {
                    Divider::Dock => drag.start_width - moved,
                    _ => drag.start_width + moved,
                };
                let window_width = f32::from(window.viewport_size().width);
                let has_dock = this.has_right_dock();
                if this.layout.resize(drag.divider, width, window_width, has_dock) {
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this: &mut Self, _ev: &MouseUpEvent, _w, cx| {
                    if this.dragging.is_some() {
                        this.end_drag();
                        cx.notify();
                    }
                }),
            )
            // Sizeless, so a click never lands on it and pulls the focus away
            // from a panel or an input.
            .child(div().track_focus(&anchor))
            .child(self.header(regions.fold, cx))
            .child(top)
            .child(body)
            .child(bottom)
    }
}

/// A toolbar button that is only an icon.
fn toolbar_button(id: &'static str, icon: Icon, color: Hsla) -> Stateful<Div> {
    div()
        .id(id)
        .p_1()
        .rounded(px(4.))
        .cursor_pointer()
        .child(sized(icon, 16., color))
}

/// A toolbar button that is only a word.
fn text_button(id: &'static str, label: &'static str, p: Palette) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .text_xs()
        .text_color(p.muted)
        .cursor_pointer()
        .child(label)
}
