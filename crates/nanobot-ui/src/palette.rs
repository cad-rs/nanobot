//! The command palette, the composer's candidate list and rewinding
//! (docs/ui-redesign-design.md §7.1, §7.4, §8).
//!
//! As elsewhere in the shell, the first half is what these *do* and runs
//! without a window; the second half draws them.

use gpui::{
    deferred, div, prelude::*, px, AnyElement, Context, Entity, MouseButton, MouseDownEvent,
    Subscription, Window,
};
use gpui_component::input::{InlineToken, Input, InputEvent, InputState};
use gpui_component::ActiveTheme as _;

use crate::activity::{action, section};
use crate::commands::{filter, trigger_at, Candidate, Command, RewindMode, Selection, Trigger};
use crate::icons::{icon, Icon};
use crate::model::{SessionId, SessionView, WorkspaceView};
use crate::shell::{palette, HarnessShell};
use crate::transcript::EntryKind;

/// The open palette: what is typed in it and which row is selected.
pub(crate) struct PaletteState {
    pub(crate) input: Entity<InputState>,
    pub(crate) query: String,
    pub(crate) selection: Selection,
    _subscription: Subscription,
}

/// The candidate list open over the composer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Completion {
    pub trigger: Trigger,
    pub candidates: Vec<Candidate>,
    pub selection: Selection,
}

/// The ids of the shell's own commands.
pub mod id {
    pub const TOGGLE_SIDEBAR: &str = "shell.toggle_sidebar";
    pub const TOGGLE_DOCK: &str = "shell.toggle_dock";
    pub const NEW_SESSION: &str = "shell.new_session";
    pub const NEXT_SESSION: &str = "shell.next_session";
    pub const PREVIOUS_SESSION: &str = "shell.previous_session";
    pub const CYCLE_PERMISSION_MODE: &str = "shell.cycle_permission_mode";
    pub const STOP_TURN: &str = "shell.stop_turn";
    pub const JUMP_TO_END: &str = "shell.jump_to_end";
    pub const TOGGLE_TOOLS: &str = "shell.toggle_tools";
    pub const TOGGLE_THEME: &str = "shell.toggle_theme";
    pub const OPEN_SETTINGS: &str = "shell.open_settings";
    pub const TOGGLE_MOTION: &str = "shell.toggle_motion";
    /// Prefix of "switch to this session"; the session's id follows.
    pub const SWITCH_SESSION: &str = "shell.switch_session:";
}

impl<S: SessionView> HarnessShell<S> {
    // ---- commands ----

    /// Adds a command of the product's to the palette.
    pub fn add_command(&mut self, command: Command, run: impl Fn() + 'static) {
        self.product_commands.push((command, std::rc::Rc::new(run)));
    }

    /// What starting a new session does (the toolbar's plus, Ctrl+N).
    pub fn set_on_new_session<F: Fn() + 'static>(&mut self, callback: F) {
        self.on_new_session = Some(std::rc::Rc::new(callback));
    }

    /// Everything the palette can run now: the shell's own commands, the
    /// product's, and a switch to each session of the open project.
    pub fn commands(&self) -> Vec<Command> {
        let mut commands = vec![
            Command::new(id::TOGGLE_SIDEBAR, self.tr("command.toggle_sidebar"))
                .with_keys("Ctrl+Alt+J"),
            Command::new(id::TOGGLE_DOCK, self.tr("command.toggle_dock")).with_keys("Ctrl+Alt+B"),
            Command::new(
                id::CYCLE_PERMISSION_MODE,
                self.tr("command.cycle_permission_mode"),
            )
            .with_keys("Shift+Tab"),
            Command::new(id::JUMP_TO_END, self.tr("command.jump_to_end")).with_keys("Ctrl+End"),
            Command::new(id::TOGGLE_TOOLS, self.tr("command.toggle_tools")).with_keys("Ctrl+O"),
            Command::new(id::NEXT_SESSION, self.tr("command.next_session")).with_keys("Ctrl+Tab"),
            Command::new(id::PREVIOUS_SESSION, self.tr("command.previous_session"))
                .with_keys("Ctrl+Shift+Tab"),
            Command::new(id::OPEN_SETTINGS, self.tr("command.open_settings")).with_keys("Ctrl+,"),
            Command::new(id::TOGGLE_THEME, self.tr("command.toggle_theme")),
            Command::new(
                id::TOGGLE_MOTION,
                self.tr(if self.reduce_motion() {
                    "command.restore_motion"
                } else {
                    "command.reduce_motion"
                }),
            ),
        ];
        if self.on_new_session.is_some() {
            commands.insert(
                0,
                Command::new(id::NEW_SESSION, self.tr("command.new_session")).with_keys("Ctrl+N"),
            );
        }
        if self.turn_phase().is_busy() {
            commands.push(Command::new(id::STOP_TURN, self.tr("command.stop_turn")).with_keys("Esc"));
        }
        commands.extend(self.product_commands.iter().map(|(command, _)| command.clone()));
        let active = self.active_session_id().cloned();
        for session in self.store().open_project_sessions() {
            if session.archived() || Some(session.id()) == active.as_ref() {
                continue;
            }
            commands.push(Command::new(
                format!("{}{}", id::SWITCH_SESSION, session.id()),
                self.trf("command.switch_session", &[("title", &session.title())]),
            ));
        }
        commands
    }

    /// The session `step` places after the one on screen in the open
    /// project, wrapping round. `None` when there is nowhere else to go.
    fn neighbour_session(&self, step: isize) -> Option<SessionId> {
        let ids: Vec<SessionId> = self
            .store()
            .open_project_sessions()
            .iter()
            .filter(|session| !session.archived())
            .map(|session| session.id().clone())
            .collect();
        let at = ids.iter().position(|id| Some(id) == self.active_session_id())?;
        let next = (at as isize + step).rem_euclid(ids.len() as isize) as usize;
        (next != at).then(|| ids[next].clone())
    }

    /// Runs the command `command`. `window_width` is for the ones that fold
    /// the layout. Changing the theme needs a window, so here it only answers
    /// true: [`run_command_in`](Self::run_command_in) does it. False when the
    /// command is unknown or cannot run now.
    pub fn run_command(&mut self, command: &str, window_width: f32) -> bool {
        match command {
            id::TOGGLE_SIDEBAR => self.toggle_sidebar(window_width),
            id::TOGGLE_DOCK => self.toggle_dock(window_width),
            id::CYCLE_PERMISSION_MODE => self.cycle_permission_mode(),
            id::JUMP_TO_END => self.stream().follow(),
            id::TOGGLE_TOOLS => return self.stream.toggle_tools(),
            id::STOP_TURN => return self.stop_turn(),
            id::TOGGLE_THEME => {}
            id::OPEN_SETTINGS => self.open_settings(),
            id::TOGGLE_MOTION => self.set_reduce_motion(!self.reduce_motion()),
            id::NEW_SESSION => match self.on_new_session.clone() {
                Some(callback) => callback(),
                None => return false,
            },
            id::NEXT_SESSION | id::PREVIOUS_SESSION => {
                let step = if command == id::NEXT_SESSION { 1 } else { -1 };
                match self.neighbour_session(step) {
                    Some(session) => return self.switch_session(&session),
                    None => return false,
                }
            }
            other => {
                if let Some(session) = other.strip_prefix(id::SWITCH_SESSION) {
                    return self.switch_session(&SessionId::new(session));
                }
                let run = self
                    .product_commands
                    .iter()
                    .find(|(command, _)| command.id == other)
                    .map(|(_, run)| run.clone());
                match run {
                    Some(run) => run(),
                    None => return false,
                }
            }
        }
        true
    }

    // ---- the composer's candidate list ----

    /// Where `@` and `/` candidates come from: the sigil and what was typed
    /// after it in, the rows to offer out. The product owns the vocabulary
    /// (its objects, its commands); the shell only shows it.
    pub fn set_completions<F>(&mut self, source: F)
    where
        F: Fn(char, &str) -> Vec<Candidate> + 'static,
    {
        self.completion_source = Some(std::rc::Rc::new(source));
    }

    /// The candidate list open over the composer, if any.
    pub fn completion(&self) -> Option<&Completion> {
        self.completion.as_ref()
    }

    /// Brings the candidate list up to the composer's `text` with the caret
    /// at byte `caret`: opened, refreshed or closed.
    pub fn update_completion(&mut self, text: &str, caret: usize) {
        let found = self.completion_source.clone().and_then(|source| {
            let trigger = trigger_at(text, caret)?;
            let candidates = source(trigger.sigil, &trigger.query);
            (!candidates.is_empty()).then_some((trigger, candidates))
        });
        self.completion = found.map(|(trigger, candidates)| {
            // Typing on keeps the row; a different trigger starts over.
            let selection = match &self.completion {
                Some(open) if open.trigger.range.start == trigger.range.start => open.selection,
                _ => Selection::default(),
            };
            Completion {
                trigger,
                candidates,
                selection,
            }
        });
    }

    /// Moves the selected candidate; false when no list is open.
    pub fn step_completion(&mut self, delta: isize) -> bool {
        match &mut self.completion {
            Some(open) => {
                let len = open.candidates.len();
                open.selection.step(delta, len);
                true
            }
            None => false,
        }
    }

    pub fn close_completion(&mut self) -> bool {
        self.completion.take().is_some()
    }

    /// Closes the list and returns what to put in the composer: the range to
    /// replace and the candidate chosen.
    pub fn accept_completion(&mut self) -> Option<(Trigger, Candidate)> {
        let open = self.completion.take()?;
        let ix = open.selection.index(open.candidates.len())?;
        Some((open.trigger, open.candidates[ix].clone()))
    }

    // ---- rewinding ----

    /// What rewinding does: the session, the event sequence of the user
    /// message to go back to, how far back, and that message's new text.
    pub fn set_on_rewind<F>(&mut self, callback: F)
    where
        F: Fn(&SessionId, u64, RewindMode, &str) + 'static,
    {
        self.on_rewind = Some(std::rc::Rc::new(callback));
    }

    /// The user message being rewound to, by its event sequence.
    pub fn rewinding(&self) -> Option<u64> {
        self.rewinding
    }

    /// Starts rewinding to the user message at `seq` and returns its text,
    /// for the composer. `None` when `seq` is not a user message or the
    /// product gave no way to rewind.
    pub fn begin_rewind(&mut self, seq: u64) -> Option<String> {
        self.on_rewind.as_ref()?;
        let text = self.transcript().entries().iter().find_map(|entry| {
            match (&entry.kind, entry.seq == seq) {
                (EntryKind::User { text }, true) => Some(text.clone()),
                _ => None,
            }
        })?;
        self.rewinding = Some(seq);
        Some(text)
    }

    pub fn cancel_rewind(&mut self) -> bool {
        self.rewinding.take().is_some()
    }

    /// The ways back the session on screen offers: restoring the product's
    /// state only where the product can.
    pub fn rewind_modes(&self) -> Vec<RewindMode> {
        let restorable = self
            .workspace()
            .is_some_and(|workspace| workspace.can_restore_state());
        let mut modes = vec![RewindMode::Conversation];
        if restorable {
            modes.push(RewindMode::ConversationAndState);
        }
        modes.push(RewindMode::Fork);
        modes
    }

    /// Rewinds to the message being rewound to, with `text` as what it says
    /// now. False when nothing is being rewound or `text` is empty.
    pub fn rewind(&mut self, mode: RewindMode, text: &str) -> bool {
        let text = text.trim();
        let (Some(seq), Some(id)) = (self.rewinding, self.active_session_id().cloned()) else {
            return false;
        };
        if text.is_empty() || !self.rewind_modes().contains(&mode) {
            return false;
        }
        self.rewinding = None;
        if let Some(callback) = self.on_rewind.clone() {
            callback(&id, seq, mode, text);
        }
        true
    }
}

// ---------------------------------------------------------------------------
// What is on screen, for tests and for the product.

impl<S: SessionView> HarnessShell<S> {

    /// What the composer holds.
    pub fn composer_text(&self, cx: &gpui::App) -> String {
        self.composer
            .as_ref()
            .map(|composer| composer.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// The ids of the references in the composer, in order.
    pub fn composer_tokens(&self, cx: &gpui::App) -> Vec<String> {
        self.composer
            .as_ref()
            .map(|composer| {
                composer
                    .read(cx)
                    .tokens()
                    .iter()
                    .map(|span| span.token().id().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn palette_is_open(&self) -> bool {
        self.palette.is_some()
    }

    /// The titles the open palette offers for what is typed in it, best first.
    pub fn palette_matches(&self) -> Vec<String> {
        let commands = self.commands();
        self.palette
            .as_ref()
            .map(|open| {
                filter(&commands, &open.query)
                    .into_iter()
                    .map(|command| command.title.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(self.tr("command.placeholder"))
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            |shell: &mut Self, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    if let Some(open) = &mut shell.palette {
                        open.query = input.read(cx).value().to_string();
                        open.selection.reset();
                        cx.notify();
                    }
                }
                InputEvent::PressEnter { .. } => shell.run_selected_command(window, cx),
                _ => {}
            },
        );
        input.update(cx, |state, cx| state.focus(window, cx));
        self.palette = Some(PaletteState {
            input,
            query: String::new(),
            selection: Selection::default(),
            _subscription: subscription,
        });
        cx.notify();
    }

    pub(crate) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.take().is_some() {
            // The keyboard goes back to the composer, where it most likely was.
            if let Some(composer) = self.composer.clone() {
                composer.update(cx, |state, cx| state.focus(window, cx));
            }
            cx.notify();
        }
    }

    pub(crate) fn step_palette(&mut self, delta: isize, cx: &mut Context<Self>) {
        let commands = self.commands();
        if let Some(open) = &mut self.palette {
            let len = filter(&commands, &open.query).len();
            open.selection.step(delta, len);
            cx.notify();
        }
    }

    fn run_selected_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let commands = self.commands();
        let chosen = self.palette.as_ref().and_then(|open| {
            let found = filter(&commands, &open.query);
            open.selection
                .index(found.len())
                .map(|ix| found[ix].id.clone())
        });
        self.close_palette(window, cx);
        if let Some(command) = chosen {
            self.run_command_in(&command, window, cx);
        }
    }

    /// Runs a command with a window to hand: the ones that need one (the
    /// theme) are finished here.
    pub(crate) fn run_command_in(
        &mut self,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let width = f32::from(window.viewport_size().width);
        self.run_command(command, width);
        if command == id::TOGGLE_THEME {
            let next = self.theme().toggled();
            self.switch_theme(next, window, cx);
        }
        cx.notify();
    }

    /// Puts the chosen candidate into the composer: a mention as one atomic
    /// token that carries the object's id, a command as its text.
    pub(crate) fn apply_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((trigger, candidate)) = self.accept_completion() else {
            return false;
        };
        let Some(composer) = self.composer.clone() else {
            return false;
        };
        composer.update(cx, |state, cx| {
            let text = format!("{}{}", trigger.sigil, candidate.label);
            if trigger.sigil == '@' {
                let token = InlineToken::new(candidate.id.clone(), text.clone())
                    .with_label(candidate.label.clone());
                if state
                    .replace_range_with_token(trigger.range.clone(), token, window, cx)
                    .is_ok()
                {
                    return;
                }
            }
            // A command, or a label a token cannot hold: plain text.
            let mut value = state.value().to_string();
            value.replace_range(trigger.range.clone(), &format!("{text} "));
            state.set_value(value, window, cx);
        });
        cx.notify();
        true
    }

    /// Starts rewinding to the user message at `seq`: its text goes back into
    /// the composer to be changed.
    pub(crate) fn begin_rewind_in(&mut self, seq: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.begin_rewind(seq) else {
            return;
        };
        if let Some(composer) = self.composer.clone() {
            composer.update(cx, |state, cx| {
                state.set_value(text, window, cx);
                state.focus(window, cx);
            });
            self.composer_has_text = true;
        }
        cx.notify();
    }

    /// Rewinds in `mode` with what the composer holds, and clears it.
    pub(crate) fn rewind_in(&mut self, mode: RewindMode, window: &mut Window, cx: &mut Context<Self>) {
        let Some(composer) = self.composer.clone() else {
            return;
        };
        let text = composer.read(cx).value().to_string();
        if self.rewind(mode, &text) {
            composer.update(cx, |state, cx| state.set_value("", window, cx));
            self.composer_has_text = false;
        }
        cx.notify();
    }

    // ---- drawing ----

    /// The palette, over everything: deferred so it also covers the viewport,
    /// with a backdrop that takes the mouse.
    pub(crate) fn palette_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.palette.as_ref()?;
        let p = palette(cx);
        let commands = self.commands();
        let found = filter(&commands, &open.query);
        let selected = open.selection.index(found.len());
        let rows = found.into_iter().enumerate().map(|(ix, command)| {
            let command_id = command.id.clone();
            div()
                .id(("harness-command", ix))
                .px_3()
                .py_1()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .text_sm()
                .cursor_pointer()
                .when(selected == Some(ix), |d| d.bg(p.accent.opacity(0.25)))
                .child(div().flex_1().min_w_0().truncate().child(command.title.clone()))
                .when_some(command.keys.clone(), |d, keys| {
                    d.child(div().text_xs().text_color(p.muted).child(keys))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                        this.close_palette(window, cx);
                        this.run_command_in(&command_id, window, cx);
                    }),
                )
        });
        let card = div()
            .id("harness-palette")
            .mt(px(96.))
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
            .child(div().p_2().child(Input::new(&open.input).w_full()))
            .child(
                div()
                    .id("harness-palette-rows")
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(p.border)
                    .py_1()
                    .children(rows)
                    .when(selected.is_none(), |d| {
                        d.child(
                            div()
                                .px_3()
                                .py_2()
                                .text_xs()
                                .text_color(p.muted)
                                .child(self.tr("command.none")),
                        )
                    }),
            );
        let backdrop = div()
            .id("harness-palette-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui::black().opacity(0.35))
            .flex()
            .flex_col()
            .items_center()
            .key_context(crate::shell::PALETTE_CONTEXT)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                    this.close_palette(window, cx);
                }),
            )
            .child(card);
        Some(deferred(backdrop).with_priority(10).into_any_element())
    }

    /// The candidate list, drawn in the column right above the composer: it
    /// never reaches over the viewport.
    pub(crate) fn completion_list(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let open = self.completion.as_ref()?;
        let p = palette(cx);
        let selected = open.selection.index(open.candidates.len());
        let sigil_icon = if open.trigger.sigil == '@' {
            Icon::Mention
        } else {
            Icon::ChevronRight
        };
        let rows = open.candidates.iter().enumerate().map(|(ix, candidate)| {
            div()
                .id(("harness-candidate", ix))
                .px_2()
                .py_1()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .text_xs()
                .cursor_pointer()
                .when(selected == Some(ix), |d| d.bg(p.accent.opacity(0.25)))
                .child(icon(sigil_icon, p.muted))
                .child(candidate.label.clone())
                .when_some(candidate.detail.clone(), |d, detail| {
                    d.child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(p.muted)
                            .child(detail),
                    )
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                        if let Some(open) = &mut this.completion {
                            open.selection = Selection::default();
                            open.selection.step(ix as isize, open.candidates.len());
                        }
                        this.apply_completion(window, cx);
                    }),
                )
        });
        Some(
            div()
                .id("harness-candidates")
                .flex_none()
                .max_h(px(180.))
                .overflow_y_scroll()
                .border_t_1()
                .border_color(p.border)
                .py_1()
                .children(rows)
                .into_any_element(),
        )
    }

    /// While a message is being rewound to: what going back can mean.
    pub(crate) fn rewind_section(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.rewinding?;
        let p = palette(cx);
        let mut buttons = div().flex().flex_row().flex_wrap().items_center().gap_2();
        for mode in self.rewind_modes() {
            let (key, color) = match mode {
                RewindMode::Conversation => ("rewind.conversation", p.fg),
                RewindMode::ConversationAndState => ("rewind.state", p.warning),
                RewindMode::Fork => ("rewind.fork", p.fg),
            };
            buttons = buttons.child(
                action(key, self.tr(key), color).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                        this.rewind_in(mode, window, cx);
                    }),
                ),
            );
        }
        buttons = buttons.child(
            div()
                .id("rewind.cancel")
                .px_2()
                .py_1()
                .text_xs()
                .text_color(p.muted)
                .cursor_pointer()
                .child(self.tr("approval.cancel"))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                        this.cancel_rewind();
                        cx.notify();
                    }),
                ),
        );
        Some(
            section(p)
                .bg(p.info.opacity(0.06))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(p.info)
                        .child(icon(Icon::Undo, p.info))
                        .child(self.tr("rewind.prompt")),
                )
                .child(buttons)
                .into_any_element(),
        )
    }
}
