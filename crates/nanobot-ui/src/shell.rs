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
//! header (identity · undo/redo · file ops)
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
    Focusable as _,
    actions, FocusHandle, KeyBinding, MouseMoveEvent, MouseUpEvent,
    div, prelude::*, px, AnyElement, App, Context, Div, Entity, Hsla, Stateful,
    IntoElement,
    MouseButton, MouseDownEvent, Render, Subscription, Window,
};
use gpui_component::{
    input::{InputEvent, InputState, TextareaState},
    ActiveTheme as _, StyledExt as _,
};
use nanobot_boot::account::Account;
use nanobot_boot::Settings;
use gpui_gpu_view::{ViewportProjection, ViewportSlot};

use crate::model::{
    PlanDecision, ProjectId,
    SessionId, SessionStatus, SessionStore, SessionView, WorkspaceView,
};
use crate::panels::{PanelArea, PanelDef, PanelHost};
use crate::commands::{Candidate, Command, RewindMode};
use crate::composer::{ApprovalDecision, Delivery, MessageQueue, PermissionMode};
use crate::palette::{Completion, PaletteState};
use crate::sidebar::Renaming;
use crate::i18n::Locale;
use crate::icons::{self, sized, Icon};
use crate::layout::{Divider, Fold, Layout, LayoutState, Stacked};
use crate::stream::StreamState;
use crate::theme::ThemeKind;
use crate::transcript::{Conversation, SessionBadge, Transcript, TurnPhase};


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
        /// Opens the command palette.
        OpenPalette,
        /// Opens the settings dialog.
        OpenSettings,
        /// Starts a new session.
        NewSession,
        /// Goes to the next session in the sidebar.
        NextSession,
        /// Goes to the previous session in the sidebar.
        PreviousSession,
        /// Scrolls the conversation to its end and follows it.
        JumpToEnd,
        ToggleTools,
        PreviousUserMessage,
        NextUserMessage,
        /// Puts the keyboard in the composer.
        FocusComposer,
        /// Moves up in the palette.
        PaletteUp,
        /// Moves down in the palette.
        PaletteDown,
        /// Closes the palette.
        PaletteClose,
        /// Puts the keyboard in the sidebar.
        FocusSidebar,
        /// Moves up in the sidebar.
        SidebarUp,
        /// Moves down in the sidebar.
        SidebarDown,
        /// Opens the session, or folds or unfolds the heading, the keyboard is on.
        SidebarOpen,
        /// Folds the heading the keyboard is on.
        SidebarFold,
        /// Unfolds the heading the keyboard is on.
        SidebarUnfold,
        /// Renames the session the keyboard is on.
        SidebarRename,
        /// Archives the session the keyboard is on.
        SidebarArchive,
        /// Puts the keyboard in the sidebar's filter.
        SidebarFilter,
    ]
);

/// The key context the shell's bindings live in.
pub const KEY_CONTEXT: &str = "HarnessShell";
/// The key context of the open command palette.
pub const PALETTE_CONTEXT: &str = "HarnessPalette";
/// The key context of the session sidebar.
pub const SIDEBAR_CONTEXT: &str = "HarnessSidebar";

/// Installs the shell's default key bindings. The product calls this once at
/// startup (after `gpui_component::init`); bindings it adds afterwards win.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-alt-j", ToggleSidebar, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-alt-b", ToggleDock, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-tab", CyclePermissionMode, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", StopTurn, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-k", OpenPalette, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-,", OpenSettings, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-n", NewSession, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-tab", NextSession, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", PreviousSession, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-end", JumpToEnd, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-o", ToggleTools, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-pageup", PreviousUserMessage, Some(KEY_CONTEXT)),
        KeyBinding::new("shift-pagedown", NextUserMessage, Some(KEY_CONTEXT)),
        KeyBinding::new("ctrl-l", FocusComposer, Some(KEY_CONTEXT)),
        KeyBinding::new("up", PaletteUp, Some(PALETTE_CONTEXT)),
        KeyBinding::new("down", PaletteDown, Some(PALETTE_CONTEXT)),
        KeyBinding::new("escape", PaletteClose, Some(PALETTE_CONTEXT)),
        KeyBinding::new("ctrl-alt-;", FocusSidebar, Some(KEY_CONTEXT)),
        KeyBinding::new("up", SidebarUp, Some(SIDEBAR_CONTEXT)),
        KeyBinding::new("down", SidebarDown, Some(SIDEBAR_CONTEXT)),
        KeyBinding::new("enter", SidebarOpen, Some(SIDEBAR_CONTEXT)),
        KeyBinding::new("left", SidebarFold, Some(SIDEBAR_CONTEXT)),
        KeyBinding::new("right", SidebarUnfold, Some(SIDEBAR_CONTEXT)),
        KeyBinding::new("f2", SidebarRename, Some(SIDEBAR_CONTEXT)),
        KeyBinding::new("shift-backspace", SidebarArchive, Some(SIDEBAR_CONTEXT)),
        KeyBinding::new("ctrl-f", SidebarFilter, Some(SIDEBAR_CONTEXT)),
    ]);
}

/// A shell callback keyed by session (activate, stop, undo, redo, save, load,
/// import, archive, fork and delete all share the shape).
pub(crate) type SessionCallback = Rc<dyn Fn(&SessionId)>;

/// The composer's submit callback.
type MessageCallback = Rc<dyn Fn(&SessionId, &str, Delivery)>;

/// The answer to a tool call waiting for approval: the session, the call's
/// event sequence and the decision.
type ApprovalCallback = Rc<dyn Fn(&SessionId, u64, ApprovalDecision)>;

/// Told when the user turns the permission dial.
type PermissionModeCallback = Rc<dyn Fn(PermissionMode)>;

/// A command of the product's in the palette, with what running it does.
type ProductCommand = (Command, Rc<dyn Fn()>);

/// Where the composer's candidates come from: the sigil and the query in.
type CompletionSource = Rc<dyn Fn(char, &str) -> Vec<Candidate>>;

/// Told to rewind: the session, the user message's event sequence, how far
/// back, and what the message says now.
type RewindCallback = Rc<dyn Fn(&SessionId, u64, RewindMode, &str)>;

/// A session and a text about it: its new title, or the object to show.
pub(crate) type SessionTextCallback = Rc<dyn Fn(&SessionId, &str)>;

/// Told to try again: the session and the error entry's sequence.
pub(crate) type RetryCallback = Rc<dyn Fn(&SessionId, u64)>;

/// A contingent-plan decision callback.
type PlanDecisionCallback = Rc<dyn Fn(&SessionId, PlanDecision)>;

/// The harness shell: the session list plus the active session's workspace.
/// A finished file picker: an open pick carries the file's bytes (already
/// read from storage), a save pick confirms the write has completed.
pub struct FilePick {
    /// True for a save picker's outcome, false for an open one.
    pub save: bool,
    /// Where the file lives (the provider id and its key).
    pub location: product_kit::Location,
    /// The file's bytes on an open pick; None on a save pick.
    pub content: Option<Vec<u8>>,
}

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
    /// The shell's own "reduce motion" switch (`ui.reduce_motion`).
    pub(crate) reduce_motion: bool,
    /// Messages held until a session's turn ends, per session.
    pub(crate) queues: HashMap<SessionId, MessageQueue>,
    /// What is unfolded in the activity bar.
    pub(crate) plan_open: bool,
    pub(crate) queue_open: bool,
    /// "Always allow" was pressed and is asking how far it should reach.
    pub(crate) always_scope_open: bool,
    /// The command palette, while it is open.
    pub(crate) palette: Option<PaletteState>,
    pub(crate) product_commands: Vec<ProductCommand>,
    pub(crate) on_new_session: Option<Rc<dyn Fn()>>,
    /// A command asked for the theme to flip; done where a window is to hand.
    /// The composer's candidate list and where its rows come from.
    pub(crate) completion_source: Option<CompletionSource>,
    pub(crate) completion: Option<Completion>,
    /// The user message being rewound to, by its event sequence.
    pub(crate) rewinding: Option<u64>,
    pub(crate) on_rewind: Option<RewindCallback>,
    /// The sidebar: which projects show their sessions, which show their
    /// long-idle ones, what is typed in the filter and where the keyboard is.
    pub(crate) expanded_projects: HashSet<ProjectId>,
    pub(crate) earlier_expanded: HashSet<ProjectId>,
    pub(crate) sidebar_query: String,
    pub(crate) sidebar_filter: Option<Entity<InputState>>,
    _sidebar_filter_sub: Option<Subscription>,
    pub(crate) sidebar_selected: Option<usize>,
    pub(crate) sidebar_focus: Option<FocusHandle>,
    pub(crate) renaming: Option<Renaming>,
    pub(crate) on_rename: Option<SessionTextCallback>,
    pub(crate) on_archive: Option<SessionCallback>,
    pub(crate) on_fork: Option<SessionCallback>,
    /// Told to try again after the error entry at this sequence.
    pub(crate) on_retry: Option<RetryCallback>,
    /// Told to show the object an entry is about.
    pub(crate) on_anchor: Option<SessionTextCallback>,
    pub(crate) on_delete: Option<SessionCallback>,
    pub(crate) session_menu: Option<crate::session_menu::SessionMenu>,
    /// Whether the settings dialog is open.
    pub(crate) settings_open: bool,
    /// The in-app file picker, when open (the web's storage has no OS
    /// dialog). The overlay draws it; `storage` answers its listings.
    pub(crate) file_picker: Option<product_kit::FilePicker>,
    /// The product's storage providers (web builds register the browser's
    /// origin-private file system).
    pub storage: product_kit::Storage,
    /// The save picker's file-name input, while a save picker is open.
    pub(crate) file_picker_name_input: Option<Entity<InputState>>,
    /// Observes a finished picker (open: the bytes; save: the write done).
    pub(crate) on_file_picked: Option<Rc<dyn Fn(FilePick)>>,
    /// The export arm: receives the storage registry, so the product packs
    /// whatever its project is (the web exports the OPFS project).
    pub(crate) on_export: Option<Rc<dyn Fn(product_kit::Storage)>>,
    /// The bytes a save picker writes, supplied by the app at confirm time.
    pub(crate) file_save_bytes: Option<Rc<dyn Fn() -> Vec<u8>>>,
    /// Callback invoked when the user decides a contingent plan.
    pub(crate) on_plan_decision: Option<PlanDecisionCallback>,
    /// Undo/redo and container save/load/import: the app layer forwards them to
    /// its engine dispatch table.
    on_undo: Option<SessionCallback>,
    on_redo: Option<SessionCallback>,
    /// An optional product projection of the sidebar (design-rooted groups
    /// instead of the flat session list). `None` = the default view.
    pub(crate) sidebar_projection: Option<Rc<dyn crate::model::SidebarProjection<S>>>,
    on_save: Option<SessionCallback>,
    on_load: Option<SessionCallback>,
    on_import: Option<SessionCallback>,
    /// Hands the current project to the creator desktop app (the manual-CAD
    /// counterpart). The shell has no project path, so the app layer resolves
    /// which member document to open.
    on_open_in_creator: Option<SessionCallback>,
    /// Keeps the composer's submit subscription alive.
    _composer_sub: Option<Subscription>,
    /// Keeps the candidate list's hold on its keys alive.
    _completion_keys: Option<Subscription>,
    /// The currently selected entity (three-way linking with the conversation).
    selected_entity: Option<String>,
    /// The currently selected conversation block event sequence.
    selected_block: Option<u64>,
    /// Product panels and script-plugin panels.
    panel_host: PanelHost<S>,
    /// The user settings store, when the app layer shared it. With it, what
    /// the settings dialog changes persists; without it the changes still
    /// apply, in memory only.
    pub(crate) settings: Option<Arc<Mutex<Settings>>>,
    /// The theme the shell renders in. Tracked so a toggle knows its
    /// starting point even without a settings store.
    theme: ThemeKind,
    /// The identity the account provider reports (`None` = a guest). The
    /// framework renders the chip; the product's callbacks decide what
    /// signing in and out mean (its own dialog, a license import) — the
    /// framework never authenticates.
    account: Option<Account>,
    on_sign_in: Option<Rc<dyn Fn()>>,
    on_sign_out: Option<Rc<dyn Fn()>>,
    /// The script plugins the app layer loaded, as the plugins panel shows
    /// them (`None` = the panel is not registered). Enabled state is
    /// runtime: the panel's disable removes a plugin's panels.
    plugins: Option<Vec<PluginEntry>>,
    /// Plugins that failed to load, with the reason.
    rejected_plugins: Vec<(String, String)>,
    /// The UI locale every string in this shell renders in: a user setting
    /// (`ui.locale`), defaulted from the harness profile.
    pub(crate) locale: Locale,
    /// The settings dialog's font-family input, created on first show.
    pub(crate) font_input: RefCell<Option<Entity<InputState>>>,
    pub(crate) font_input_sub: RefCell<Option<Subscription>>,
    /// How the window is divided: the saved preference and what the window's
    /// width does to it now.
    pub(crate) layout: LayoutState,
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
    pub(crate) unread: RefCell<HashSet<SessionId>>,
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
            reduce_motion: false,
            queues: HashMap::new(),
            plan_open: false,
            queue_open: true,
            always_scope_open: false,
            palette: None,
            product_commands: Vec::new(),
            on_new_session: None,
            completion_source: None,
            completion: None,
            rewinding: None,
            on_rewind: None,
            expanded_projects: HashSet::new(),
            earlier_expanded: HashSet::new(),
            sidebar_query: String::new(),
            sidebar_filter: None,
            _sidebar_filter_sub: None,
            sidebar_selected: None,
            sidebar_focus: None,
            renaming: None,
            on_rename: None,
            on_archive: None,
            on_fork: None,
            on_retry: None,
            on_anchor: None,
            on_delete: None,
            session_menu: None,
            settings_open: false,
            file_picker: None,
            file_picker_name_input: None,
            on_file_picked: None,
            file_save_bytes: None,
            on_export: None,
            storage: {
                let mut storage = product_kit::Storage::default();
                // Web builds get the browser's origin-private file system as
                // the in-app picker's place; the desktop registers its own
                // sources (none today: the OS dialogs still rule there).
                #[cfg(target_family = "wasm")]
                storage.register("opfs", std::sync::Arc::new(product_kit::opfs::Opfs));
                storage
            },
            on_message_sent: None,
            on_plan_decision: None,
            sidebar_projection: None,
            on_undo: None,
            on_redo: None,
            on_save: None,
            on_load: None,
            on_import: None,
            on_open_in_creator: None,
            _composer_sub: None,
            _completion_keys: None,
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
            font_input: RefCell::new(None),
            font_input_sub: RefCell::new(None),
            layout: LayoutState::default(),
            dragging: None,
            focus_anchor: None,
            transcript: Transcript::default(),
            stream: StreamState::default(),
            unread: RefCell::new(HashSet::new()),
        };
        // The open project starts unfolded; the others wait to be asked.
        let open = shell.store.borrow().open_project_id().clone();
        shell.expanded_projects.insert(open);
        // Framework chrome: the diagnostics panel is always there (it reads
        // nothing but shell state).
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
                } => {
                    // Enter belongs to whatever is open over the composer.
                    if shell.completion.is_some() {
                        shell.apply_completion(window, cx);
                    } else if shell.rewinding.is_some() {
                        shell.rewind_in(RewindMode::Conversation, window, cx);
                    } else {
                        shell.submit_composer(*secondary, window, cx);
                    }
                }
                InputEvent::Change => {
                    let (text, caret, in_token) = {
                        let state = input.read(cx);
                        let caret = state.cursor();
                        // A reference already made is not one in the making.
                        let in_token = state.tokens().iter().any(|span| {
                            let range = span.range();
                            range.start < caret && caret <= range.end
                        });
                        (state.value().to_string(), caret, in_token)
                    };
                    // The candidate list follows what is typed at the caret,
                    // and the button whether there is anything to send.
                    let was_open = shell.completion.is_some();
                    if in_token {
                        shell.close_completion();
                    } else {
                        shell.update_completion(&text, caret);
                    }
                    let has_text = !text.trim().is_empty();
                    if shell.composer_has_text != has_text
                        || was_open
                        || shell.completion.is_some()
                    {
                        shell.composer_has_text = has_text;
                        cx.notify();
                    }
                }
                _ => {}
            },
        );

        // While the candidate list is open, the arrows, Tab, Enter and Escape
        // are its.
        // The text area has bindings of its own for them and sits deeper in
        // the focus path, so a key context here would lose to it: the keys are
        // taken before any binding is looked up.
        let weak = cx.weak_entity();
        let keys = cx.intercept_keystrokes(move |event, window, cx| {
            if event.keystroke.modifiers.modified() {
                return;
            }
            let key = event.keystroke.key.clone();
            if !matches!(key.as_str(), "up" | "down" | "tab" | "enter" | "escape") {
                return;
            }
            let Some(shell) = weak.upgrade() else {
                return;
            };
            let taken = shell.update(cx, |shell, cx| {
                let typing = shell
                    .composer
                    .as_ref()
                    .is_some_and(|composer| composer.focus_handle(cx).is_focused(window));
                if shell.completion.is_none() || !typing {
                    return false;
                }
                match key.as_str() {
                    "up" => {
                        shell.step_completion(-1);
                    }
                    "down" => {
                        shell.step_completion(1);
                    }
                    "tab" | "enter" => {
                        shell.apply_completion(window, cx);
                    }
                    _ => {
                        shell.close_completion();
                    }
                }
                cx.notify();
                true
            });
            if taken {
                cx.stop_propagation();
            }
        });

        // The sidebar's filter: what is typed narrows the rows as it is typed.
        let filter = cx.new(|cx| {
            InputState::new(window, cx).placeholder(crate::i18n::t(locale, "sidebar.filter"))
        });
        let filter_sub = cx.subscribe_in(
            &filter,
            window,
            |shell: &mut Self, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    shell.set_sidebar_filter(input.read(cx).value().to_string());
                    cx.notify();
                }
            },
        );

        let mut shell = Self::new(store);
        shell.locale = locale;
        shell.composer = Some(composer);
        shell._composer_sub = Some(subscription);
        shell._completion_keys = Some(keys);
        shell.sidebar_filter = Some(filter);
        shell._sidebar_filter_sub = Some(filter_sub);
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
        self.on_session_activated = Some(Rc::new(callback));
    }

    pub fn set_on_message_sent<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId, &str, Delivery) + 'static,
    {
        self.on_message_sent = Some(Rc::new(callback));
    }

    /// What stopping the turn in progress does (the composer's button while
    /// the agent works, or Escape).
    pub fn set_on_stop<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_stop = Some(Rc::new(callback));
    }

    /// What answering a tool call that waits for approval does: the session,
    /// the call's event sequence and the user's decision.
    pub fn set_on_approval<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId, u64, ApprovalDecision) + 'static,
    {
        self.on_approval = Some(Rc::new(callback));
    }

    /// What turning the permission dial does. The shell shows and remembers
    /// the mode; what each mode lets the agent do is the product's policy.
    pub fn set_on_permission_mode<F>(&mut self, callback: F)
    where
        F: Fn(PermissionMode) + 'static,
    {
        self.on_permission_mode = Some(Rc::new(callback));
    }

    pub fn set_on_plan_decision<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId, PlanDecision) + 'static,
    {
        self.on_plan_decision = Some(Rc::new(callback));
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
        self.on_undo = Some(Rc::new(callback));
    }

    pub fn set_on_redo<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_redo = Some(Rc::new(callback));
    }

    pub fn set_on_save<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_save = Some(Rc::new(callback));
    }

    pub fn set_on_load<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_load = Some(Rc::new(callback));
    }

    pub fn set_on_import<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_import = Some(Rc::new(callback));
    }

    /// Opens the current project in the creator desktop app. The shell only
    /// knows the session, so the app layer resolves the project's member
    /// document and hands it over.
    pub fn set_on_open_in_creator<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId) + 'static,
    {
        self.on_open_in_creator = Some(Rc::new(callback));
    }

    /// Registers the export arm: called with the storage registry when the
    /// user asks for the project package (the web packs the OPFS project).
    pub fn set_on_export<F>(&mut self, callback: F)
    where
        F: Fn(product_kit::Storage) + 'static,
    {
        self.on_export = Some(Rc::new(callback));
    }

    /// The bytes a save picker writes, supplied at confirm time (the app
    /// serialises whatever "the document" means for it). Without this, a
    /// save pick reports "nothing to save".
    pub fn set_file_save_bytes<F>(&mut self, provide: F)
    where
        F: Fn() -> Vec<u8> + 'static,
    {
        self.file_save_bytes = Some(Rc::new(provide));
    }

    /// Shares the harness's user settings with the shell: the layout, the
    /// language, the permission dial and the motion switch are read from it,
    /// and what the settings dialog changes is written back to it.
    pub fn set_settings(&mut self, settings: Option<Arc<Mutex<Settings>>>) {
        if let Some(store) = &settings {
            if let Ok(store) = store.lock() {
                self.theme = crate::theme::kind_from_settings(&store);
                if let Some(code) = store.get(crate::theme::LOCALE_KEY).and_then(|v| v.as_str()) {
                    self.locale = Locale::from_code(code);
                }
                self.layout = LayoutState::new(Layout::load(&store));
                self.permission_mode = PermissionMode::from_code(
                    &store.get_str(crate::activity::PERMISSION_MODE_KEY, ""),
                );
                self.reduce_motion = store
                    .get(crate::motion::REDUCE_MOTION_KEY)
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
            }
        }
        self.settings = settings;
    }

    /// The shared settings store, when the app layer provided one.
    pub fn settings(&self) -> Option<Arc<Mutex<Settings>>> {
        self.settings.clone()
    }

    /// The settings file's path as the settings dialog and the diagnostics
    /// panel show it (`None` = in-memory, nothing shared).
    pub fn settings_path(&self) -> Option<String> {
        self.settings
            .as_ref()
            .and_then(|store| store.lock().ok())
            .and_then(|settings| settings.location())
    }

    /// The theme the shell renders in.
    pub fn theme(&self) -> ThemeKind {
        self.theme
    }

    /// The UI locale. Switch it with [`HarnessShell::set_locale`] (the
    /// settings dialog does, through `ui.locale`).
    pub fn locale(&self) -> Locale {
        self.locale
    }

    /// Switches the UI locale; the shell re-renders on the next frame.
    pub fn set_locale(&mut self, locale: Locale) {
        self.locale = locale;
    }

    /// Writes one font setting and re-themes the window. The app layer applies
    /// the stored fonts at launch through [`crate::theme::apply_font`].
    pub(crate) fn set_font_setting(
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
    pub(crate) fn switch_theme(
        &mut self,
        kind: ThemeKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
        self.on_sign_in = Some(Rc::new(callback));
    }

    /// What the chip's sign-out affordance does.
    pub fn set_on_sign_out<F>(&mut self, callback: F)
    where
        F: Fn() + 'static,
    {
        self.on_sign_out = Some(Rc::new(callback));
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

    /// Takes the conversation to show. When it differs from the one on
    /// screen, the list is told what changed as rows to remeasure and a
    /// splice, so a streaming answer costs its last row, not the whole stream.
    fn show_conversation(&mut self, conversation: Conversation) {
        if self.transcript.replace(conversation) {
            self.stream.set_entries(self.transcript.entries());
        }
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
        self.show_conversation(workspace.conversation());
        self.workspace = Some(workspace);
        // On screen now: it is no longer news.
        self.unread.borrow_mut().remove(id);
        if let Some(ref callback) = self.on_session_activated {
            callback(id);
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
        self.show_conversation(workspace.conversation());
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
        self.expanded_projects.insert(id.clone());
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

    /// Writes one setting through the settings store, when the app layer
    /// shared one. A failed write loses the preference, never the UI.
    pub(crate) fn save_setting(&self, key: &str, value: serde_json::Value) {
        if let Some(Ok(mut settings)) = self.settings.as_ref().map(|s| s.lock()) {
            let _ = settings.set(key, value);
        }
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
            gpui_gpu_view::suspend_viewport_input(false);
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
                    gpui_gpu_view::suspend_viewport_input(true);
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
                        // And the keyboard lands on who is waiting.
                        this.select_first_waiting();
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
            // What the turn is doing right now, then the session's status.
            .children(workspace.and_then(|_| self.turn_mark(self.turn_phase(), "harness-turn-mark", cx)))
            .when_some(status, |d, status| {
                d.child(badge(status.label(self.locale), session_color(status, p)))
            })
            .when(self.on_new_session.is_some(), |d| {
                d.child(
                    toolbar_button("harness-new-session", Icon::Plus, p.muted).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                            this.run_command_in(crate::palette::id::NEW_SESSION, window, cx);
                        }),
                    ),
                )
            })
            // Everything else the shell can do is in the palette.
            .child(
                toolbar_button("harness-open-palette", Icon::More, p.muted).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                        this.open_palette(window, cx);
                    }),
                ),
            )
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
            .child(self.session_buttons(cx))
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

    /// The operations on the session on screen: undo, redo, save, load,
    /// import. Each calls the product's callback, when there is one —
    /// except save / load when in-app storage sources exist (the web's
    /// OPFS): those open the in-app file picker instead, whose finished
    /// choice arrives through [`Self::set_on_file_picked`].
    fn session_buttons(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let session = self
            .workspace
            .as_ref()
            .map_or_else(|| SessionId::new(""), |ws| ws.session_id().clone());
        let buttons = [
            ("harness-undo", Icon::Undo, "shell.undo", &self.on_undo),
            ("harness-redo", Icon::Redo, "shell.redo", &self.on_redo),
            ("harness-import", Icon::Import, "shell.import", &self.on_import),
            (
                "harness-open-in-creator",
                Icon::External,
                "shell.creator",
                &self.on_open_in_creator,
            ),
        ];
        let picker_sources = self.storage.in_app_sources();
        let has_picker = !picker_sources.is_empty();
        let mut row = div()
            .flex()
            .flex_row()
            .gap_2()
            .children(buttons.map(|(id, icon, label, callback)| {
                let (callback, session) = (callback.clone(), session.clone());
                header_button(id, icon, self.tr(label), p.fg, p.border).on_click(move |_, _, _| {
                    if let Some(callback) = &callback {
                        callback(&session);
                    }
                })
            }));

        let seed = format!("{}.jsonl", session);
        let session_save = session.clone();
        row = row.child(
            header_button("harness-save", Icon::Save, self.tr("shell.save"), p.fg, p.border)
                .on_click(cx.listener(move |this, _ev, window, cx| {
                    if has_picker {
                        this.open_file_picker(
                            Some(&seed),
                            Vec::new(),
                            window,
                            cx,
                        );
                    } else if let Some(callback) = &this.on_save {
                        callback(&session_save);
                    }
                })),
        );
        let session_load = session.clone();
        row = row.child(
            header_button("harness-load", Icon::Load, self.tr("shell.load"), p.fg, p.border)
                .on_click(cx.listener(move |this, _ev, window, cx| {
                    if has_picker {
                        this.open_file_picker(None, Vec::new(), window, cx);
                    } else if let Some(callback) = &this.on_load {
                        callback(&session_load);
                    }
                })),
        );
        // The export arm (web): pack the project out of in-app storage.
        row = row.child(
            header_button("harness-export", Icon::Export, self.tr("shell.export"), p.fg, p.border)
                .on_click(cx.listener(move |this, _, _, cx| {
                    if has_picker {
                        if let Some(callback) = &this.on_export {
                            callback(this.storage.clone());
                        }
                    }
                })),
        );
        row
    }

    pub(crate) fn sidebar_footer(&self, p: Palette, cx: &mut Context<Self>) -> impl IntoElement {
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
            .child(toolbar_button("harness-settings", Icon::Settings, p.muted).on_click(
                cx.listener(|shell, _event, _window, cx| {
                    shell.open_settings();
                    cx.notify();
                }),
            ))
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
                            sign_out();
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
                        sign_in();
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
            .children(self.rewind_section(cx))
            .children(self.completion_list(cx))
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
                    self.panel_host.script_element(&panel.id, script, self.locale, window, cx)
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

/// A header button: an icon and a word.
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



impl<S: SessionView> Render for HarnessShell<S> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        // Bring the conversation on screen up to the workspace's projection.
        let conversation = self
            .workspace
            .as_ref()
            .map(|ws| ws.conversation())
            .unwrap_or_default();
        self.show_conversation(conversation);
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
        // The sidebar takes the keyboard as a whole: its rows are moved
        // through with the arrows.
        if self.sidebar_focus.is_none() {
            self.sidebar_focus = Some(cx.focus_handle());
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
                .child(self.render_sidebar(width, cx))
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
                            .child(div().flex_1().min_h_0().child(self.viewport_slot()))
                            // Alone in the window, the viewport keeps a way
                            // to talk to the session under it.
                            .when(regions.column.is_none(), |d| {
                                d.child(self.composer_strip(ws, cx))
                            }),
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
                // An open menu or dialog takes Escape first.
                if this.close_settings() || this.close_session_menu() {
                    cx.notify();
                    return;
                }
                // Escape with nothing running is for whatever else wants it.
                if this.stop_turn() {
                    cx.notify();
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this: &mut Self, _: &OpenPalette, window, cx| {
                this.open_palette(window, cx);
            }))
            .on_action(cx.listener(|this: &mut Self, _: &OpenSettings, _window, cx| {
                this.open_settings();
                cx.notify();
            }))
            .on_action(cx.listener(|this: &mut Self, _: &NewSession, window, cx| {
                this.run_command_in(crate::palette::id::NEW_SESSION, window, cx);
            }))
            .on_action(cx.listener(|this: &mut Self, _: &NextSession, window, cx| {
                this.run_command_in(crate::palette::id::NEXT_SESSION, window, cx);
            }))
            .on_action(cx.listener(|this: &mut Self, _: &PreviousSession, window, cx| {
                this.run_command_in(crate::palette::id::PREVIOUS_SESSION, window, cx);
            }))
            .on_action(cx.listener(|this: &mut Self, _: &JumpToEnd, window, cx| {
                this.run_command_in(crate::palette::id::JUMP_TO_END, window, cx);
            }))
            .on_action(cx.listener(|this: &mut Self, _: &ToggleTools, window, cx| {
                this.run_command_in(crate::palette::id::TOGGLE_TOOLS, window, cx);
            }))
            .on_action(cx.listener(|this: &mut Self, _: &PreviousUserMessage, _window, cx| {
                this.stream.step_user_message(-1);
                cx.notify();
            }))
            .on_action(cx.listener(|this: &mut Self, _: &NextUserMessage, _window, cx| {
                this.stream.step_user_message(1);
                cx.notify();
            }))
            .on_action(cx.listener(|this: &mut Self, _: &FocusComposer, window, cx| {
                if let Some(composer) = this.composer.clone() {
                    composer.update(cx, |state, cx| state.focus(window, cx));
                }
            }))
            .on_action(cx.listener(|this: &mut Self, _: &PaletteUp, _window, cx| {
                this.step_palette(-1, cx);
            }))
            .on_action(cx.listener(|this: &mut Self, _: &PaletteDown, _window, cx| {
                this.step_palette(1, cx);
            }))
            .on_action(cx.listener(|this: &mut Self, _: &PaletteClose, window, cx| {
                this.close_palette(window, cx);
            }))
            .on_action(cx.listener(|this: &mut Self, _: &FocusSidebar, window, cx| {
                // A hidden sidebar is shown first.
                let width = f32::from(window.viewport_size().width);
                if this.layout.reveal_sidebar(width) {
                    this.save_layout();
                }
                if let Some(focus) = this.sidebar_focus.clone() {
                    window.focus(&focus, cx);
                }
                if this.sidebar_selected.is_none() {
                    this.step_sidebar(1);
                }
                cx.notify();
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
            .children(self.session_menu_overlay(window, cx))
            .children(self.file_picker_overlay(window, cx))
            .children(self.settings_overlay(window, cx))
            .children(self.palette_overlay(cx))
    }
}

/// A toolbar button that is only an icon.
fn toolbar_button(id: &'static str, icon: Icon, color: Hsla) -> Stateful<Div> {
    div()        .id(id)
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
